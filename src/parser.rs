//! Parser for `.korin` files.
//!
//! Parsing happens in two passes. The first splits the file into lines and builds a tree from
//! their indentation. The second scans each line with a cursor that knows its byte offset, so
//! every node and every error carries an exact span.

use std::sync::Arc;

use crate::ast::{
    Call, Capture, CaptureMode, CmpOp, Command, CommandKind, Define, Duration, Expect, Expr,
    GateMode, Header, Ident, Interpolated, Match, Module, Param, ParamKind, Part, Poll, Ref,
    SayKind, Span, Spanned, Stage, Step, Use, UseTarget, Wizard,
};
use crate::diag::{Diag, SourceFile};

type PResult<T> = Result<T, Diag>;

/// The longest duration korin accepts: 30 days. Deadlines are `Instant::now() + duration`, so
/// a duration must stay far from overflow.
const MAX_SECONDS: u64 = 30 * 24 * 3600;

/// Parses a wizard file.
///
/// # Errors
///
/// Returns every syntax error found. The parser recovers at the next line, so one call
/// reports as many errors as it can.
pub fn parse_wizard(file: &Arc<SourceFile>) -> Result<Wizard, Vec<Diag>> {
    let mut parser = Parser::new(file);
    let nodes = parser.tree()?;
    let wizard = parser.wizard(&nodes);
    parser.finish(wizard)
}

/// Parses a module file, which holds only `use` and `define`.
///
/// # Errors
///
/// Returns every syntax error found.
pub fn parse_module(file: &Arc<SourceFile>) -> Result<Module, Vec<Diag>> {
    let mut parser = Parser::new(file);
    let nodes = parser.tree()?;
    let module = parser.module(&nodes);
    parser.finish(module)
}

#[derive(Debug, Clone, Copy)]
struct Line {
    indent: usize,
    start: usize,
    end: usize,
}

#[derive(Debug)]
struct Node {
    line: Line,
    children: Vec<Node>,
}

impl Node {
    fn span(&self) -> Span {
        Span::new(self.line.start, self.line.end)
    }
}

struct Parser<'a> {
    file: &'a Arc<SourceFile>,
    errors: Vec<Diag>,
}

impl<'a> Parser<'a> {
    fn new(file: &'a Arc<SourceFile>) -> Self {
        Self {
            file,
            errors: Vec::new(),
        }
    }

    fn finish<T>(self, value: Option<T>) -> Result<T, Vec<Diag>> {
        match value {
            Some(value) if self.errors.is_empty() => Ok(value),
            _ => Err(self.errors),
        }
    }

    fn err(&self, code: &'static str, span: Span, message: impl Into<String>) -> Diag {
        Diag::error(self.file, code, span, message)
    }

    fn cur(&self, node: &Node) -> Cur<'a> {
        Cur {
            file: self.file,
            src: &self.file.text,
            pos: node.line.start,
            end: node.line.end,
        }
    }

    // ---- pass 1: lines and indentation ----

    fn lines(&mut self) -> Vec<Line> {
        let text = &self.file.text;
        let mut lines = Vec::new();
        let mut offset = 0;
        for raw in text.split_inclusive('\n') {
            let line_start = offset;
            offset += raw.len();
            let raw = raw.trim_end_matches(['\n', '\r']);
            let indent = raw.len() - raw.trim_start_matches([' ', '\t']).len();
            if let Some(tab) = raw[..indent].find('\t') {
                let at = line_start + tab;
                self.errors.push(
                    self.err(
                        "tab",
                        Span::new(at, at + 1),
                        "tabs are not allowed in indentation",
                    )
                    .label("tab here")
                    .help("indent with spaces"),
                );
                continue;
            }
            let content = strip_comment(&raw[indent..]).trim_end();
            if content.is_empty() {
                continue;
            }
            lines.push(Line {
                indent,
                start: line_start + indent,
                end: line_start + indent + content.len(),
            });
        }
        lines
    }

    fn tree(&mut self) -> Result<Vec<Node>, Vec<Diag>> {
        let lines = self.lines();
        let mut index = 0;
        let nodes = take_level(&lines, &mut index, 0);
        if let Some(line) = lines.get(index) {
            let span = Span::new(line.start, line.end);
            self.errors.push(
                self.err(
                    "indent",
                    span,
                    "this line's indentation does not match any enclosing block",
                )
                .label("unexpected indentation")
                .help("indent each block consistently, deeper than the line that opens it"),
            );
        }
        if self.errors.is_empty() {
            Ok(nodes)
        } else {
            Err(std::mem::take(&mut self.errors))
        }
    }

    // ---- pass 2: files ----

    fn wizard(&mut self, nodes: &[Node]) -> Option<Wizard> {
        let Some((first, rest)) = nodes.split_first() else {
            self.errors.push(
                self.err("header", Span::new(0, 0), "the file is empty")
                    .help("a wizard starts with `korin 1`"),
            );
            return None;
        };
        let version = self.recover(|p| p.version_line(first))?;
        let mut title: Option<Spanned<String>> = None;
        let mut gates: Option<(GateMode, Span)> = None;
        let mut unattended: Option<Span> = None;
        let mut allow: Option<(Vec<Spanned<String>>, Span)> = None;
        let mut uses = Vec::new();
        let mut params = Vec::new();
        let mut stages = Vec::new();

        for node in rest {
            let mut cur = self.cur(node);
            let Some(keyword) = cur.peek_ident() else {
                self.errors.push(self.err(
                    "syntax",
                    node.span(),
                    "expected a keyword such as `stage`",
                ));
                continue;
            };
            let header_keyword =
                matches!(keyword.as_str(), "title" | "gates" | "unattended" | "allow");
            if header_keyword && !stages.is_empty() {
                self.errors.push(self.err(
                    "misplaced",
                    node.span(),
                    format!("`{keyword}` belongs in the header, before the first stage"),
                ));
                continue;
            }
            let result = match keyword.as_str() {
                "title" => self.no_children(node).and_then(|()| {
                    cur.expect_keyword("title")?;
                    let text = cur.plain_string()?;
                    cur.expect_end()?;
                    set_once(self.file, &mut title, text, node.span(), "title")
                }),
                "gates" => self.no_children(node).and_then(|()| {
                    cur.expect_keyword("gates")?;
                    let mode = match cur.expect_ident("`all` or `mutating`")? {
                        Spanned { value, .. } if value == "all" => GateMode::All,
                        Spanned { value, .. } if value == "mutating" => GateMode::Mutating,
                        other => {
                            return Err(self
                                .err("syntax", other.span, "expected `all` or `mutating`"));
                        }
                    };
                    cur.expect_end()?;
                    set_once(self.file, &mut gates, (mode, node.span()), node.span(), "gates")
                }),
                "unattended" => self.no_children(node).and_then(|()| {
                    cur.expect_keyword("unattended")?;
                    cur.expect_keyword("allowed")?;
                    cur.expect_end()?;
                    set_once(self.file, &mut unattended, node.span(), node.span(), "unattended")
                }),
                "allow" => self.no_children(node).and_then(|()| {
                    cur.expect_keyword("allow")?;
                    let mut names = Vec::new();
                    loop {
                        let word = cur.word(&[','])?;
                        let Some(text) = word.as_literal() else {
                            return Err(self.err(
                                "syntax",
                                word.span,
                                "`allow` takes literal names or paths, not interpolations",
                            ));
                        };
                        names.push(Spanned::new(text, word.span));
                        if !cur.eat_char(',') {
                            break;
                        }
                    }
                    cur.expect_end()?;
                    set_once(self.file, &mut allow, (names, node.span()), node.span(), "allow")
                }),
                "use" => self.no_children(node).and_then(|()| {
                    uses.push(self.use_line(node)?);
                    Ok(())
                }),
                "param" => self.no_children(node).and_then(|()| {
                    params.push(self.param_line(node)?);
                    Ok(())
                }),
                "stage" => {
                    if let Some(stage) = self.stage(node) {
                        stages.push(stage);
                    }
                    Ok(())
                }
                "define" => Err(self
                    .err("misplaced", node.span(), "`define` belongs in a module file")
                    .help("move templates into a file and import it with `use \"./lib/name.korin\"`")),
                "korin" => Err(self.err("header", node.span(), "`korin` must appear only once, on the first line")),
                other => Err(self
                    .err("syntax", node.span(), format!("unexpected `{other}` at the top level"))
                    .help("top-level lines are header lines, `use`, `param` or `stage`; steps go indented under a stage")),
            };
            if let Err(diag) = result {
                self.errors.push(diag);
            }
        }

        let Some(title) = title else {
            self.errors.push(
                self.err("header", first.span(), "the wizard has no `title`")
                    .help("add `title \"What this wizard does\"` after `korin 1`"),
            );
            return None;
        };
        if stages.is_empty() {
            self.errors
                .push(self.err("header", first.span(), "the wizard has no stages"));
            return None;
        }
        Some(Wizard {
            header: Header {
                version,
                title,
                gates: gates.map(|(mode, _)| mode).unwrap_or_default(),
                unattended,
                allow: allow.map(|(names, _)| names),
            },
            uses,
            params,
            stages,
        })
    }

    fn module(&mut self, nodes: &[Node]) -> Option<Module> {
        #![allow(clippy::unnecessary_wraps)]
        let mut uses = Vec::new();
        let mut defines = Vec::new();
        for node in nodes {
            let cur = self.cur(node);
            let result = match cur.peek_ident().as_deref() {
                Some("use") => self.no_children(node).and_then(|()| {
                    uses.push(self.use_line(node)?);
                    Ok(())
                }),
                Some("define") => {
                    if let Some(define) = self.define(node) {
                        defines.push(define);
                    }
                    Ok(())
                }
                _ => Err(self
                    .err(
                        "misplaced",
                        node.span(),
                        "a module file holds only `use` and `define`",
                    )
                    .help("stages, params and header lines belong in the wizard file")),
            };
            if let Err(diag) = result {
                self.errors.push(diag);
            }
        }
        Some(Module { uses, defines })
    }

    fn recover<T>(&mut self, f: impl FnOnce(&mut Self) -> PResult<T>) -> Option<T> {
        match f(self) {
            Ok(value) => Some(value),
            Err(diag) => {
                self.errors.push(diag);
                None
            }
        }
    }

    fn no_children(&self, node: &Node) -> PResult<()> {
        match node.children.first() {
            None => Ok(()),
            Some(child) => Err(self
                .err(
                    "indent",
                    child.span(),
                    "this line is indented under a line that takes no body",
                )
                .label("unexpected block")),
        }
    }

    fn version_line(&mut self, node: &Node) -> PResult<Spanned<u32>> {
        let mut cur = self.cur(node);
        if cur.peek_ident().as_deref() != Some("korin") {
            return Err(self
                .err("header", node.span(), "a wizard must start with `korin 1`")
                .label("expected `korin 1` here"));
        }
        self.no_children(node)?;
        cur.expect_keyword("korin")?;
        let number = cur.integer()?;
        cur.expect_end()?;
        if number.value != 1 {
            return Err(self
                .err(
                    "header",
                    number.span,
                    format!("unsupported language version {}", number.value),
                )
                .help("this korin understands version 1"));
        }
        Ok(Spanned::new(1, number.span))
    }

    fn use_line(&self, node: &Node) -> PResult<Use> {
        let mut cur = self.cur(node);
        cur.expect_keyword("use")?;
        let target = if cur.peek() == Some('"') {
            UseTarget::Path(cur.plain_string()?)
        } else {
            UseTarget::Builtin(cur.expect_ident("a module name or a quoted path")?)
        };
        let alias = if cur.eat_keyword("as").is_some() {
            Some(cur.expect_ident("a module alias")?)
        } else {
            None
        };
        cur.expect_end()?;
        Ok(Use {
            target,
            alias,
            span: node.span(),
        })
    }

    fn param_line(&self, node: &Node) -> PResult<Param> {
        let mut cur = self.cur(node);
        cur.expect_keyword("param")?;
        let name = cur.expect_ident("a param name")?;
        let kind = if cur.eat_char('=') {
            ParamKind::Default(cur.string()?)
        } else if cur.eat_keyword("required").is_some() {
            ParamKind::Required
        } else if cur.eat_keyword("secret").is_some() {
            ParamKind::Secret
        } else {
            return Err(cur.error_here("expected `required`, `secret` or `= \"default\"`"));
        };
        cur.expect_end()?;
        Ok(Param {
            name,
            kind,
            span: node.span(),
        })
    }

    fn stage(&mut self, node: &Node) -> Option<Stage> {
        let title = self.recover(|p| {
            let mut cur = p.cur(node);
            cur.expect_keyword("stage")?;
            let title = cur.plain_string()?;
            cur.expect_end()?;
            Ok(title)
        })?;
        if node.children.is_empty() {
            self.errors
                .push(self.err("syntax", node.span(), "the stage has no steps"));
            return None;
        }
        let steps = self.steps(&node.children);
        Some(Stage {
            title,
            steps,
            span: node.span(),
        })
    }

    fn define(&mut self, node: &Node) -> Option<Define> {
        let (name, params) = self.recover(|p| {
            let mut cur = p.cur(node);
            cur.expect_keyword("define")?;
            let name = cur.expect_ident("a template name")?;
            cur.expect_char('(', "`(`")?;
            let mut params = Vec::new();
            if !cur.eat_char(')') {
                loop {
                    params.push(cur.expect_ident("a parameter name")?);
                    if cur.eat_char(')') {
                        break;
                    }
                    cur.expect_char(',', "`,` or `)`")?;
                }
            }
            cur.expect_end()?;
            Ok((name, params))
        })?;
        let mut children = node.children.as_slice();
        let mut exports = Vec::new();
        if let Some(first) = children.first()
            && self.cur(first).peek_ident().as_deref() == Some("exports")
        {
            let parsed = self.recover(|p| {
                p.no_children(first)?;
                let mut cur = p.cur(first);
                cur.expect_keyword("exports")?;
                let mut names = vec![cur.expect_ident("an exported name")?];
                while cur.eat_char(',') {
                    names.push(cur.expect_ident("an exported name")?);
                }
                cur.expect_end()?;
                Ok(names)
            });
            exports = parsed.unwrap_or_default();
            children = &children[1..];
        }
        if children.is_empty() {
            self.errors
                .push(self.err("syntax", node.span(), "the template has no steps"));
            return None;
        }
        let body = self.steps(children);
        Some(Define {
            name,
            params,
            exports,
            body,
            span: node.span(),
        })
    }

    // ---- steps ----

    fn steps(&mut self, nodes: &[Node]) -> Vec<Step> {
        nodes
            .iter()
            .filter_map(|node| self.recover(|p| p.step(node)))
            .collect()
    }

    fn body(&mut self, node: &Node, what: &str) -> PResult<Vec<Step>> {
        if node.children.is_empty() {
            return Err(self
                .err(
                    "syntax",
                    node.span(),
                    format!("`{what}` needs an indented body"),
                )
                .label("no body"));
        }
        Ok(self.steps(&node.children))
    }

    fn step(&mut self, node: &Node) -> PResult<Step> {
        let mut cur = self.cur(node);
        let Some(keyword) = cur.peek_ident() else {
            return Err(cur.error_here("expected a step"));
        };
        let span = node.span();
        match keyword.as_str() {
            "read" | "mutate" | "interactive" => {
                cur.expect_keyword(&keyword)?;
                let kind = match keyword.as_str() {
                    "read" => CommandKind::Read,
                    "mutate" => CommandKind::Mutate,
                    _ => CommandKind::Interactive,
                };
                self.command(node, cur, kind)
            }
            "script" => {
                cur.expect_keyword("script")?;
                let kind = match cur.expect_ident("`read` or `mutate`")? {
                    Spanned { value, .. } if value == "read" => CommandKind::ScriptRead,
                    Spanned { value, .. } if value == "mutate" => CommandKind::ScriptMutate,
                    other => {
                        return Err(self
                            .err(
                                "syntax",
                                other.span,
                                "expected `script read` or `script mutate`",
                            )
                            .help("the keyword says whether the script changes state"));
                    }
                };
                self.command(node, cur, kind)
            }
            "confirm" => {
                self.no_children(node)?;
                cur.expect_keyword("confirm")?;
                let text = cur.string()?;
                cur.expect_end()?;
                Ok(Step::Confirm { text, span })
            }
            "ask" => {
                self.no_children(node)?;
                cur.expect_keyword("ask")?;
                let secret = cur.eat_keyword("secret").is_some();
                let name = cur.expect_ident("a name for the answer")?;
                let prompt = cur.string()?;
                cur.expect_end()?;
                Ok(Step::Ask {
                    name,
                    secret,
                    prompt,
                    span,
                })
            }
            "assert" => {
                self.no_children(node)?;
                cur.expect_keyword("assert")?;
                let cond = cur.expr()?;
                cur.expect_keyword("else").map_err(|d| {
                    d.help("write `assert <condition> else \"message shown when it fails\"`")
                })?;
                let message = cur.string()?;
                cur.expect_end()?;
                Ok(Step::Assert {
                    cond,
                    message,
                    span,
                })
            }
            "when" => {
                cur.expect_keyword("when")?;
                let cond = cur.expr()?;
                cur.expect_end()?;
                let body = self.body(node, "when")?;
                Ok(Step::When { cond, body, span })
            }
            "each" => {
                cur.expect_keyword("each")?;
                let var = cur.expect_ident("a loop variable")?;
                cur.expect_keyword("in")?;
                let list = cur.expr()?;
                cur.expect_end()?;
                let body = self.body(node, "each")?;
                Ok(Step::Each {
                    var,
                    list,
                    body,
                    span,
                })
            }
            "poll" => self.poll(node, cur),
            "note" | "warn" | "record" => {
                self.no_children(node)?;
                cur.expect_keyword(&keyword)?;
                let kind = match keyword.as_str() {
                    "note" => SayKind::Note,
                    "warn" => SayKind::Warn,
                    _ => SayKind::Record,
                };
                let text = cur.string()?;
                cur.expect_end()?;
                Ok(Step::Say { kind, text, span })
            }
            "until" | "fail" => Err(self.err(
                "misplaced",
                span,
                format!("`{keyword}` belongs directly inside a `poll`"),
            )),
            "capture" | "expect" | "env" | "timeout" => Err(self.err(
                "misplaced",
                span,
                format!("`{keyword}` is an option and belongs indented under a command"),
            )),
            "exports" => Err(self.err(
                "misplaced",
                span,
                "`exports` must be the first line of a `define` body",
            )),
            _ => {
                if cur.is_call() {
                    self.no_children(node)?;
                    Ok(Step::Call(cur.call(span)?))
                } else {
                    Err(self
                        .err("unknown-step", span, format!("unknown step `{keyword}`"))
                        .help("steps are read, mutate, script, interactive, confirm, ask, assert, when, each, poll, note, warn, record, or a template call like `rds.cluster({id}) as c`"))
                }
            }
        }
    }

    fn command(&mut self, node: &Node, mut cur: Cur<'_>, kind: CommandKind) -> PResult<Step> {
        let mut argv = Vec::new();
        while !cur.at_end() {
            argv.push(cur.word(&[])?);
        }
        if argv.is_empty() {
            return Err(cur.error_here("expected a command"));
        }
        let mut command = Command {
            kind,
            argv,
            captures: Vec::new(),
            expects: Vec::new(),
            env: Vec::new(),
            timeout: None,
            span: node.span(),
        };
        for child in &node.children {
            if let Err(diag) = self.option(child, &mut command) {
                self.errors.push(diag);
            }
        }
        Ok(Step::Command(command))
    }

    fn option(&mut self, node: &Node, command: &mut Command) -> PResult<()> {
        self.no_children(node)?;
        let mut cur = self.cur(node);
        let span = node.span();
        match cur.peek_ident().as_deref() {
            Some("capture") => {
                cur.expect_keyword("capture")?;
                let secret = cur.eat_keyword("secret").is_some();
                let name = cur.expect_ident("a capture name")?;
                let optional = cur.eat_char_tight('?');
                cur.expect_char('=', "`=`")?;
                let file = if cur.eat_keyword("file").is_some() {
                    Some(cur.word(&[])?)
                } else {
                    None
                };
                let (mode, on) = cur.capture_mode()?;
                command.captures.push(Capture {
                    name,
                    secret,
                    optional,
                    file,
                    mode,
                    on,
                    span,
                });
            }
            Some("expect") => {
                cur.expect_keyword("expect")?;
                let name = cur.expect_ident("a name for the `expect`")?;
                cur.expect_char('=', "`=`")?;
                let mut matches = vec![cur.expect_match()?];
                while cur.eat_keyword("and").is_some() {
                    matches.push(cur.expect_match()?);
                }
                cur.expect_end()?;
                command.expects.push(Expect {
                    name,
                    matches,
                    span,
                });
            }
            Some("env") => {
                cur.expect_keyword("env")?;
                let name = cur.expect_ident("an environment variable name")?;
                if !cur.eat_char_tight('=') {
                    return Err(cur.error_here(
                        "expected `=` right after the name, as in `env NAME={value}`",
                    ));
                }
                let value = cur.word(&[])?;
                cur.expect_end()?;
                command.env.push((name, value));
            }
            Some("timeout") => {
                cur.expect_keyword("timeout")?;
                let duration = cur.duration()?;
                cur.expect_end()?;
                if command.timeout.is_some() {
                    return Err(self.err("syntax", span, "the command already has a `timeout`"));
                }
                command.timeout = Some(duration);
            }
            _ => {
                return Err(self
                    .err(
                        "syntax",
                        span,
                        "expected a command option: `capture`, `expect`, `env` or `timeout`",
                    )
                    .help("steps are not indented under a command; dedent this line"));
            }
        }
        Ok(())
    }

    fn poll(&mut self, node: &Node, mut cur: Cur<'_>) -> PResult<Step> {
        cur.expect_keyword("poll")?;
        cur.expect_keyword("every")?;
        let every = cur.duration()?;
        cur.expect_keyword("for")?;
        let limit = cur.duration()?;
        cur.expect_end()?;
        let mut body_nodes = Vec::new();
        let mut until: Option<Expr> = None;
        let mut fail: Option<Expr> = None;
        for child in &node.children {
            let mut child_cur = self.cur(child);
            match child_cur.peek_ident().as_deref() {
                Some(kw @ ("until" | "fail")) => {
                    let kw = kw.to_string();
                    let parsed = self.recover(|p| {
                        p.no_children(child)?;
                        child_cur.expect_keyword(&kw)?;
                        let expr = child_cur.expr()?;
                        child_cur.expect_end()?;
                        Ok(expr)
                    });
                    let Some(expr) = parsed else { continue };
                    let slot = if kw == "until" { &mut until } else { &mut fail };
                    if slot.is_some() {
                        self.errors.push(self.err(
                            "syntax",
                            child.span(),
                            format!("the poll already has `{kw}`"),
                        ));
                    } else {
                        *slot = Some(expr);
                    }
                }
                _ => body_nodes.push(child),
            }
        }
        let body: Vec<Step> = body_nodes
            .into_iter()
            .filter_map(|child| self.recover(|p| p.step(child)))
            .collect();
        let Some(until) = until else {
            return Err(self
                .err("syntax", node.span(), "the poll has no `until` condition")
                .help("add an indented `until <condition>` line"));
        };
        if body.is_empty() {
            return Err(self.err("syntax", node.span(), "the poll has no steps to repeat"));
        }
        Ok(Step::Poll(Poll {
            every,
            limit,
            body,
            until,
            fail,
            span: node.span(),
        }))
    }
}

fn set_once<T>(
    file: &Arc<SourceFile>,
    slot: &mut Option<T>,
    value: T,
    span: Span,
    what: &str,
) -> PResult<()> {
    if slot.is_some() {
        return Err(Diag::error(
            file,
            "header",
            span,
            format!("`{what}` appears more than once"),
        ));
    }
    *slot = Some(value);
    Ok(())
}

fn take_level(lines: &[Line], index: &mut usize, indent: usize) -> Vec<Node> {
    let mut nodes = Vec::new();
    while let Some(line) = lines.get(*index) {
        if line.indent != indent {
            break;
        }
        *index += 1;
        let children = match lines.get(*index) {
            Some(next) if next.indent > indent => take_level(lines, index, next.indent),
            _ => Vec::new(),
        };
        nodes.push(Node {
            line: *line,
            children,
        });
    }
    nodes
}

/// Removes a `#` comment. A `#` starts a comment at the start of the content or after
/// whitespace, outside double quotes.
fn strip_comment(content: &str) -> &str {
    let mut in_quote = false;
    let mut escaped = false;
    let mut previous_space = true;
    for (index, ch) in content.char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            in_quote = !in_quote;
        } else if ch == '#' && !in_quote && previous_space {
            return &content[..index];
        }
        previous_space = ch.is_whitespace();
    }
    content
}

fn is_ident_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

fn is_ident_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// A cursor over one line.
struct Cur<'a> {
    file: &'a Arc<SourceFile>,
    src: &'a str,
    pos: usize,
    end: usize,
}

impl Cur<'_> {
    fn err(&self, span: Span, message: impl Into<String>) -> Diag {
        Diag::error(self.file, "syntax", span, message)
    }

    fn error_here(&self, message: impl Into<String>) -> Diag {
        let end = self.next_char_end();
        self.err(Span::new(self.pos, end), message)
    }

    fn next_char_end(&self) -> usize {
        self.peek_raw()
            .map_or(self.pos, |ch| (self.pos + ch.len_utf8()).min(self.end))
    }

    fn peek_raw(&self) -> Option<char> {
        if self.pos >= self.end {
            return None;
        }
        self.src[self.pos..self.end].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek_raw()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn skip_ws(&mut self) {
        while let Some(ch) = self.peek_raw() {
            if !ch.is_whitespace() {
                break;
            }
            self.pos += ch.len_utf8();
        }
    }

    fn at_end(&mut self) -> bool {
        self.skip_ws();
        self.pos >= self.end
    }

    fn peek(&mut self) -> Option<char> {
        self.skip_ws();
        self.peek_raw()
    }

    fn rest_text(&self) -> &str {
        &self.src[self.pos..self.end]
    }

    fn eat_char(&mut self, ch: char) -> bool {
        if self.peek() == Some(ch) {
            self.pos += ch.len_utf8();
            true
        } else {
            false
        }
    }

    /// Eats `ch` only if it comes immediately, with no whitespace before it.
    fn eat_char_tight(&mut self, ch: char) -> bool {
        if self.peek_raw() == Some(ch) {
            self.pos += ch.len_utf8();
            true
        } else {
            false
        }
    }

    fn expect_char(&mut self, ch: char, what: &str) -> PResult<Span> {
        self.skip_ws();
        let start = self.pos;
        if self.eat_char(ch) {
            Ok(Span::new(start, self.pos))
        } else {
            Err(self.error_here(format!("expected {what}")))
        }
    }

    fn expect_end(&mut self) -> PResult<()> {
        if self.at_end() {
            Ok(())
        } else {
            let span = Span::new(self.pos, self.end);
            Err(self
                .err(span, "unexpected text at the end of the line")
                .label("not expected here"))
        }
    }

    fn peek_ident(&self) -> Option<String> {
        let mut probe = Cur {
            file: self.file,
            src: self.src,
            pos: self.pos,
            end: self.end,
        };
        probe.ident().map(|ident| ident.value)
    }

    fn ident(&mut self) -> Option<Ident> {
        self.skip_ws();
        let start = self.pos;
        let first = self.peek_raw()?;
        if !is_ident_start(first) {
            return None;
        }
        while let Some(ch) = self.peek_raw() {
            if !is_ident_char(ch) {
                break;
            }
            self.pos += 1;
        }
        Some(Spanned::new(
            self.src[start..self.pos].to_string(),
            Span::new(start, self.pos),
        ))
    }

    fn expect_ident(&mut self, what: &str) -> PResult<Ident> {
        self.skip_ws();
        self.ident()
            .ok_or_else(|| self.error_here(format!("expected {what}")))
    }

    fn eat_keyword(&mut self, keyword: &str) -> Option<Span> {
        if self.peek_ident().as_deref() == Some(keyword) {
            self.ident().map(|ident| ident.span)
        } else {
            None
        }
    }

    fn expect_keyword(&mut self, keyword: &str) -> PResult<Span> {
        self.skip_ws();
        self.eat_keyword(keyword)
            .ok_or_else(|| self.error_here(format!("expected `{keyword}`")))
    }

    fn integer(&mut self) -> PResult<Spanned<i64>> {
        self.skip_ws();
        let start = self.pos;
        if self.peek_raw() == Some('-') {
            self.pos += 1;
        }
        while self.peek_raw().is_some_and(|ch| ch.is_ascii_digit()) {
            self.pos += 1;
        }
        let text = &self.src[start..self.pos];
        text.parse()
            .map(|value| Spanned::new(value, Span::new(start, self.pos)))
            .map_err(|_| {
                self.pos = start;
                self.error_here("expected a whole number")
            })
    }

    fn duration(&mut self) -> PResult<Spanned<Duration>> {
        self.skip_ws();
        let start = self.pos;
        while self.peek_raw().is_some_and(|ch| ch.is_ascii_digit()) {
            self.pos += 1;
        }
        let digits = &self.src[start..self.pos];
        let unit = self.bump();
        let span = Span::new(start, self.pos);
        let factor = match unit {
            Some('s') => 1,
            Some('m') => 60,
            Some('h') => 3600,
            _ => 0,
        };
        let followed_by_word = self.peek_raw().is_some_and(is_ident_char);
        let well_formed = factor > 0 && !followed_by_word && !digits.is_empty();
        // Digits too large for `u64` parse as an error, so they count as too long as well.
        let seconds = digits.parse::<u64>().ok().map(|n| n.checked_mul(factor));
        match seconds {
            Some(Some(n)) if well_formed && (1..=MAX_SECONDS).contains(&n) => {
                Ok(Spanned::new(Duration(n), span))
            }
            Some(Some(0)) if well_formed => {
                Err(self.err(span, "a duration must be longer than zero"))
            }
            _ if well_formed => Err(self
                .err(span, "this duration is too long")
                .help("a duration can be at most 30 days (`720h`)")),
            _ => Err(self.err(
                Span::new(start, self.pos.max(start + 1).min(self.end)),
                "expected a duration such as `30s`, `15m` or `2h`",
            )),
        }
    }

    /// A double-quoted string with no interpolation.
    fn plain_string(&mut self) -> PResult<Spanned<String>> {
        let template = self.string()?;
        match template.as_literal() {
            Some(text) => Ok(Spanned::new(text, template.span)),
            None => Err(self.err(
                template.span,
                "this string cannot contain `{...}` interpolations",
            )),
        }
    }

    /// A double-quoted string, with `{name}` interpolation.
    fn string(&mut self) -> PResult<Interpolated> {
        self.skip_ws();
        let start = self.pos;
        if self.peek_raw() != Some('"') {
            return Err(self.error_here("expected a double-quoted string"));
        }
        self.pos += 1;
        let mut parts = Vec::new();
        self.quoted_body(&mut parts, start)?;
        Ok(Interpolated {
            parts: merge_literals(parts),
            span: Span::new(start, self.pos),
        })
    }

    /// Reads up to and including the closing quote. The opening quote is already consumed.
    fn quoted_body(&mut self, parts: &mut Vec<Part>, open: usize) -> PResult<()> {
        loop {
            match self.peek_raw() {
                None => {
                    return Err(self
                        .err(Span::new(open, self.end), "this string is not closed")
                        .label("missing closing `\"`"));
                }
                Some('"') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some('\\') => {
                    self.pos += 1;
                    parts.push(Part::Lit(self.escape()?));
                }
                Some('{') => parts.push(Part::Ref(self.reference()?)),
                Some(ch) => {
                    self.pos += ch.len_utf8();
                    parts.push(Part::Lit(ch.to_string()));
                }
            }
        }
    }

    fn escape(&mut self) -> PResult<String> {
        let start = self.pos - 1;
        match self.bump() {
            Some('n') => Ok("\n".into()),
            Some('t') => Ok("\t".into()),
            Some(ch @ ('"' | '\\' | '{' | '}' | ' ' | '#' | ',' | ')')) => Ok(ch.to_string()),
            _ => Err(self
                .err(Span::new(start, self.pos), "unknown escape")
                .help("valid escapes are \\\" \\\\ \\{ \\} \\n \\t and \\ followed by a space")),
        }
    }

    /// `{name}`, `{alias.export}` or `{list...}`.
    fn reference(&mut self) -> PResult<Ref> {
        let start = self.pos;
        self.pos += 1;
        let mut path = Vec::new();
        while let Some(ch) = self.peek_raw() {
            if !is_ident_start(ch) {
                break;
            }
            let ident_start = self.pos;
            while self.peek_raw().is_some_and(is_ident_char) {
                self.pos += 1;
            }
            path.push(self.src[ident_start..self.pos].to_string());
            if self.rest_text().starts_with("...") || self.peek_raw() != Some('.') {
                break;
            }
            self.pos += 1;
        }
        let splat = if self.rest_text().starts_with("...") {
            self.pos += 3;
            true
        } else {
            false
        };
        if path.is_empty() || self.peek_raw() != Some('}') {
            let end = self.src[self.pos..self.end]
                .find('}')
                .map_or(self.end, |i| self.pos + i + 1);
            return Err(self
                .err(Span::new(start, end), "invalid interpolation")
                .help("write `{name}`, `{alias.name}` or `{list...}`; escape a literal brace as `\\{`"));
        }
        self.pos += 1;
        Ok(Ref {
            path,
            splat,
            span: Span::new(start, self.pos),
        })
    }

    /// One whitespace-separated word, which may mix literal text, `"quoted"` segments and
    /// `{name}` interpolations. Stops at whitespace or any character in `stops`.
    fn word(&mut self, stops: &[char]) -> PResult<Interpolated> {
        self.skip_ws();
        let start = self.pos;
        let mut parts = Vec::new();
        while let Some(ch) = self.peek_raw() {
            if ch.is_whitespace() || stops.contains(&ch) {
                break;
            }
            match ch {
                '"' => {
                    let open = self.pos;
                    self.pos += 1;
                    self.quoted_body(&mut parts, open)?;
                }
                '\\' => {
                    self.pos += 1;
                    parts.push(Part::Lit(self.escape()?));
                }
                '{' => parts.push(Part::Ref(self.reference()?)),
                _ => {
                    self.pos += ch.len_utf8();
                    parts.push(Part::Lit(ch.to_string()));
                }
            }
        }
        if self.pos == start {
            return Err(self.error_here("expected a word"));
        }
        Ok(Interpolated {
            parts: merge_literals(parts),
            span: Span::new(start, self.pos),
        })
    }

    fn regex(&mut self) -> PResult<Spanned<String>> {
        self.skip_ws();
        let start = self.pos;
        if self.peek_raw() != Some('/') {
            return Err(self.error_here("expected a regex such as `/NotFound/`"));
        }
        self.pos += 1;
        let mut pattern = String::new();
        loop {
            match self.bump() {
                None => {
                    return Err(self
                        .err(Span::new(start, self.end), "this regex is not closed")
                        .label("missing closing `/`"));
                }
                Some('/') => break,
                Some('\\') if self.peek_raw() == Some('/') => {
                    self.pos += 1;
                    pattern.push('/');
                }
                Some('\\') => {
                    pattern.push('\\');
                    if let Some(next) = self.bump() {
                        pattern.push(next);
                    }
                }
                Some(ch) => pattern.push(ch),
            }
        }
        Ok(Spanned::new(pattern, Span::new(start, self.pos)))
    }

    fn capture_mode(&mut self) -> PResult<(Spanned<CaptureMode>, Option<Ident>)> {
        self.skip_ws();
        let start = self.pos;
        let mode = self.expect_ident("a capture mode: `jq`, `text`, `lines` or `regex`")?;
        match mode.value.as_str() {
            "jq" => {
                let variant = match self.peek_ident().as_deref() {
                    Some("list") => {
                        self.ident();
                        Some("list")
                    }
                    Some("json") => {
                        self.ident();
                        Some("json")
                    }
                    _ => None,
                };
                self.skip_ws();
                let filter_start = self.pos;
                let (filter, on) = split_trailing_on(self.rest_text(), filter_start);
                if on.is_none()
                    && let Some(span) = misplaced_on_match(self.rest_text(), filter_start)
                {
                    return Err(self.on_takes_a_name(span));
                }
                self.pos = self.end;
                if filter.is_empty() {
                    return Err(self.err(Span::new(filter_start, filter_start + 1), "expected a jq filter"));
                }
                let span = Span::new(start, filter_start + filter.len());
                let mode = match variant {
                    Some("list") => CaptureMode::JqList(filter),
                    Some("json") => CaptureMode::JqJson(filter),
                    _ => CaptureMode::Jq(filter),
                };
                Ok((Spanned::new(mode, span), on))
            }
            "text" | "lines" => {
                let mode = if mode.value == "text" {
                    CaptureMode::Text
                } else {
                    CaptureMode::Lines
                };
                let on = self.trailing_on()?;
                let span = mode_span(start, &mode);
                Ok((Spanned::new(mode, span), on))
            }
            "regex" => {
                let re = self.regex()?;
                let on = self.trailing_on()?;
                Ok((
                    Spanned::new(CaptureMode::Regex(re.value), Span::new(start, re.span.end)),
                    on,
                ))
            }
            other => Err(self
                .err(mode.span, format!("unknown capture mode `{other}`"))
                .help("use `jq <filter>`, `jq list <filter>`, `jq json <filter>`, `text`, `lines` or `regex /re/`")),
        }
    }

    fn trailing_on(&mut self) -> PResult<Option<Ident>> {
        let on = if self.eat_keyword("on").is_some() {
            let name = self.expect_ident("the name of an `expect` on this step")?;
            if MATCH_KEYWORDS.contains(&name.value.as_str()) && !self.at_end() {
                return Err(self.on_takes_a_name(name.span));
            }
            Some(name)
        } else {
            None
        };
        self.expect_end()?;
        Ok(on)
    }

    /// The error for `on exit 0` and the like: `on` names an `expect`, it is not a match.
    fn on_takes_a_name(&self, span: Span) -> Diag {
        self.err(span, "`on` takes the name of an `expect`, not a match")
            .label("this is a match")
            .help("a plain capture already runs only when the command succeeds; to capture when an `expect` matches, declare it on the step, as in `expect gone = exit 254`, then write `on gone`")
    }

    fn expect_match(&mut self) -> PResult<Spanned<Match>> {
        self.skip_ws();
        let start = self.pos;
        let keyword = self.expect_ident("`exit`, `signal`, `stdout`, `stderr` or `missing`")?;
        let expect_match = match keyword.value.as_str() {
            "exit" => {
                if self.rest_text().trim_start().starts_with("!=") {
                    self.skip_ws();
                    self.pos += 2;
                    let zero = self.integer()?;
                    if zero.value != 0 {
                        return Err(self.err(zero.span, "only `exit != 0` is supported"));
                    }
                    Match::ExitNonZero
                } else {
                    let code = self.integer()?;
                    let value = i32::try_from(code.value)
                        .ok()
                        .filter(|code| (0..=255).contains(code))
                        .ok_or_else(|| self.err(code.span, "an exit code is between 0 and 255"))?;
                    Match::Exit(value)
                }
            }
            "signal" => {
                let name = self.expect_ident("a signal name such as `TERM`")?;
                let bare = name.value.trim_start_matches("SIG").to_string();
                if crate::signals::number(&bare).is_none() {
                    return Err(self.err(name.span, format!("unknown signal `{}`", name.value)));
                }
                Match::Signal(bare)
            }
            "stdout" | "stderr" => {
                self.expect_char('~', "`~`")?;
                let re = self.regex()?;
                if keyword.value == "stdout" {
                    Match::Stdout(re.value)
                } else {
                    Match::Stderr(re.value)
                }
            }
            "missing" => Match::Missing(self.word(&[])?),
            other => {
                return Err(self
                    .err(keyword.span, format!("unknown `expect` match `{other}`"))
                    .help("matches are `exit <n>`, `exit != 0`, `signal <NAME>`, `stdout ~ /re/`, `stderr ~ /re/` and `missing <path>`"));
            }
        };
        Ok(Spanned::new(expect_match, Span::new(start, self.pos)))
    }

    // ---- template calls ----

    fn is_call(&self) -> bool {
        let text = self.rest_text().trim_start();
        let name_len = text.chars().take_while(|c| is_ident_char(*c)).count();
        text[name_len..].starts_with('.')
    }

    fn call(&mut self, span: Span) -> PResult<Call> {
        let module = self.expect_ident("a module name")?;
        if !self.eat_char_tight('.') {
            return Err(self.error_here("expected `.` between the module and the template"));
        }
        let template = self.expect_ident("a template name")?;
        self.expect_char('(', "`(`")?;
        let mut args = Vec::new();
        if !self.eat_char(')') {
            loop {
                args.push(self.word(&[',', ')'])?);
                if self.eat_char(')') {
                    break;
                }
                self.expect_char(',', "`,` or `)`")?;
            }
        }
        let alias = if self.eat_keyword("as").is_some() {
            Some(self.expect_ident("an alias")?)
        } else {
            None
        };
        self.expect_end()?;
        Ok(Call {
            module,
            template,
            args,
            alias,
            span,
        })
    }

    // ---- expressions ----

    fn expr(&mut self) -> PResult<Expr> {
        let mut left = self.and_expr()?;
        while self.eat_keyword("or").is_some() {
            let right = self.and_expr()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn and_expr(&mut self) -> PResult<Expr> {
        let mut left = self.not_expr()?;
        while self.eat_keyword("and").is_some() {
            let right = self.not_expr()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn not_expr(&mut self) -> PResult<Expr> {
        if let Some(span) = self.eat_keyword("not") {
            let inner = self.not_expr()?;
            return Ok(Expr::Not(Box::new(inner), span));
        }
        self.cmp_expr()
    }

    fn cmp_expr(&mut self) -> PResult<Expr> {
        let left = self.primary()?;
        self.skip_ws();
        let rest = self.rest_text();
        let op = [
            ("==", CmpOp::Eq),
            ("!=", CmpOp::Ne),
            ("<=", CmpOp::Le),
            (">=", CmpOp::Ge),
            ("<", CmpOp::Lt),
            (">", CmpOp::Gt),
        ]
        .into_iter()
        .find(|(text, _)| rest.starts_with(text));
        if let Some((text, op)) = op {
            self.pos += text.len();
            let right = self.primary()?;
            return Ok(Expr::Cmp(op, Box::new(left), Box::new(right)));
        }
        if rest.starts_with('~') {
            self.pos += 1;
            let re = self.regex()?;
            return Ok(Expr::Regex(Box::new(left), re));
        }
        if self.eat_keyword("in").is_some() {
            let right = self.primary()?;
            return Ok(Expr::In(Box::new(left), Box::new(right)));
        }
        Ok(left)
    }

    fn primary(&mut self) -> PResult<Expr> {
        self.skip_ws();
        let start = self.pos;
        match self.peek_raw() {
            Some('(') => {
                self.pos += 1;
                let inner = self.expr()?;
                self.expect_char(')', "`)`")?;
                Ok(inner)
            }
            Some('"') => Ok(Expr::Str(self.string()?)),
            Some('[') => {
                self.pos += 1;
                let mut items = Vec::new();
                if !self.eat_char(']') {
                    loop {
                        items.push(self.primary()?);
                        if self.eat_char(']') {
                            break;
                        }
                        self.expect_char(',', "`,` or `]`")?;
                    }
                }
                Ok(Expr::List(items, Span::new(start, self.pos)))
            }
            Some(ch) if ch.is_ascii_digit() || ch == '-' => {
                if ch == '-' {
                    self.pos += 1;
                }
                while self
                    .peek_raw()
                    .is_some_and(|c| c.is_ascii_digit() || c == '.')
                {
                    self.pos += 1;
                }
                let text = &self.src[start..self.pos];
                if text.parse::<f64>().is_err() {
                    return Err(self.err(Span::new(start, self.pos), "expected a number"));
                }
                Ok(Expr::Num(Spanned::new(
                    text.to_string(),
                    Span::new(start, self.pos),
                )))
            }
            Some(ch) if is_ident_start(ch) => {
                if self.eat_keyword("exists").is_some() {
                    return Ok(Expr::Exists(self.bare_ref()?));
                }
                let word = self.peek_ident().unwrap_or_default();
                if matches!(word.as_str(), "and" | "or" | "in" | "else" | "not") {
                    return Err(self.error_here(format!("expected a value before `{word}`")));
                }
                Ok(Expr::Ref(self.bare_ref()?))
            }
            _ => Err(self.error_here("expected a value: a name, a string, a number or a list")),
        }
    }

    fn bare_ref(&mut self) -> PResult<Ref> {
        self.skip_ws();
        let start = self.pos;
        let mut path = vec![self.expect_ident("a name")?.value];
        while self.peek_raw() == Some('.') {
            self.pos += 1;
            path.push(self.expect_ident("a name after `.`")?.value);
        }
        Ok(Ref {
            path,
            splat: false,
            span: Span::new(start, self.pos),
        })
    }
}

fn mode_span(start: usize, mode: &CaptureMode) -> Span {
    let len = match mode {
        CaptureMode::Text => 4,
        _ => 5,
    };
    Span::new(start, start + len)
}

/// The keywords that start an `expect` match.
const MATCH_KEYWORDS: [&str; 5] = ["exit", "signal", "stdout", "stderr", "missing"];

/// Finds `on <match keyword> ...` in a jq filter, as in `jq .id on exit 0`, and returns the
/// span of the match keyword. jq has no `on` keyword, so this is never a real filter.
fn misplaced_on_match(text: &str, offset: usize) -> Option<Span> {
    let mut words = text
        .split_whitespace()
        .map(|word| (word.as_ptr() as usize - text.as_ptr() as usize, word));
    let mut previous: Option<&str> = None;
    for (start, word) in words.by_ref() {
        if previous == Some("on") && MATCH_KEYWORDS.contains(&word) {
            return Some(Span::new(offset + start, offset + start + word.len()));
        }
        previous = Some(word);
    }
    None
}

/// Splits a trailing ` on <expect>` off a jq filter.
fn split_trailing_on(text: &str, offset: usize) -> (String, Option<Ident>) {
    let trimmed = text.trim_end();
    let mut tokens = trimmed.rsplitn(3, char::is_whitespace);
    let last = tokens.next();
    let before = tokens.next();
    if let (Some(name), Some("on")) = (last, before)
        && name.chars().next().is_some_and(is_ident_start)
        && name.chars().all(is_ident_char)
    {
        let name_start = offset + trimmed.len() - name.len();
        let filter = trimmed[..trimmed.len() - name.len()]
            .trim_end()
            .strip_suffix("on")
            .unwrap_or_default()
            .trim_end()
            .to_string();
        let ident = Spanned::new(
            name.to_string(),
            Span::new(name_start, name_start + name.len()),
        );
        return (filter, Some(ident));
    }
    (trimmed.to_string(), None)
}

fn merge_literals(parts: Vec<Part>) -> Vec<Part> {
    let mut merged: Vec<Part> = Vec::with_capacity(parts.len());
    for part in parts {
        if let (Part::Lit(text), Some(Part::Lit(previous))) = (&part, merged.last_mut()) {
            previous.push_str(text);
            continue;
        }
        merged.push(part);
    }
    merged
}
