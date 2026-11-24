//! The user's config file: reader preferences, such as the view a run starts in.
//!
//! The file is `$XDG_CONFIG_HOME/korin/config.kdl`, else `~/.config/korin/config.kdl`, or the
//! path in `KORIN_CONFIG`. It is the operator's file only: no project file can change how a
//! reviewer sees a gate. A missing file is not an error.
//!
//! Each key is a KDL v2 node with one argument, such as `view compact`. For every key a flag
//! wins over the environment, the environment over the file, and the file over the default.

use std::fmt;
use std::path::{Path, PathBuf};

use kdl::{KdlDocument, KdlNode, KdlValue};

use crate::ast::Span;
use crate::diag::{Diag, SourceFile};
use crate::theme::{Choice, Icons};
use crate::view::View;

/// The keys a config file may set, in the order `korin config` lists them.
pub const KEYS: &[&str] = &[
    "view",
    "legend",
    "script-lines",
    "pager",
    "require-enter",
    "color",
    "icons",
    "open",
];

/// The kinds of item the `o` picker opens, each with its own opener in an `open` block.
pub const OPEN_KINDS: &[&str] = &["script", "json", "markdown", "text"];

/// A loaded config file. Each key the file sets keeps its value.
#[derive(Debug, Default)]
pub struct Config {
    /// Where the file is, or would be.
    pub path: PathBuf,
    /// Whether the file exists.
    pub exists: bool,
    pub view: Option<View>,
    pub legend: Option<bool>,
    pub script_lines: Option<usize>,
    pub pager: Option<String>,
    pub require_enter: Option<bool>,
    pub color: Option<Choice>,
    pub icons: Option<Icons>,
    /// The `open` block: a command for each kind of item it names.
    pub open: Vec<(&'static str, String)>,
}

/// Where a key's effective value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Flag,
    Env,
    File,
    Default,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Source::Flag => "flag",
            Source::Env => "env",
            Source::File => "file",
            Source::Default => "default",
        })
    }
}

/// The values given on the command line, which win over everything else.
#[derive(Debug, Default, Clone, Copy)]
pub struct Flags {
    pub require_enter: bool,
    pub script_lines: Option<usize>,
    pub color: Option<Choice>,
}

/// Every key's effective value, with where it came from.
#[derive(Debug, Clone)]
pub struct Effective {
    pub view: (View, Source),
    pub legend: (bool, Source),
    pub script_lines: (usize, Source),
    /// The pager command, if the file sets one; otherwise `$PAGER`, else `less`.
    pub pager: (Option<String>, Source),
    pub require_enter: (bool, Source),
    pub color: (Choice, Source),
    pub icons: (Icons, Source),
    /// The opener for each kind in [`OPEN_KINDS`], if one is set; otherwise the pager.
    pub open: Vec<(&'static str, Option<String>, Source)>,
}

impl Effective {
    /// The command that opens an item of `kind`, if one is set.
    pub fn opener(&self, kind: &str) -> Option<&str> {
        self.open
            .iter()
            .find(|(k, _, _)| *k == kind)
            .and_then(|(_, command, _)| command.as_deref())
    }
}

impl Effective {
    /// Each key with its value as the file would write it, for `korin config`.
    pub fn listing(&self) -> Vec<(&'static str, String, Source)> {
        let mut list = vec![
            ("view", view_name(self.view.0).to_string(), self.view.1),
            (
                "legend",
                bool_name(self.legend.0).to_string(),
                self.legend.1,
            ),
            (
                "script-lines",
                self.script_lines.0.to_string(),
                self.script_lines.1,
            ),
            (
                "pager",
                self.pager
                    .0
                    .as_ref()
                    .map_or_else(|| "$PAGER, else less".to_string(), |p| format!("{p:?}")),
                self.pager.1,
            ),
            (
                "require-enter",
                bool_name(self.require_enter.0).to_string(),
                self.require_enter.1,
            ),
            ("color", choice_name(self.color.0).to_string(), self.color.1),
            (
                "icons",
                match self.icons.0 {
                    Icons::Plain => "plain",
                    Icons::Nerd => "nerd",
                }
                .to_string(),
                self.icons.1,
            ),
        ];
        for (kind, command, source) in &self.open {
            list.push((
                // Each kind lists on its own line, under the block's name.
                match *kind {
                    "script" => "open.script",
                    "json" => "open.json",
                    "markdown" => "open.markdown",
                    _ => "open.text",
                },
                command
                    .as_ref()
                    .map_or_else(|| "the pager".to_string(), |c| format!("{c:?}")),
                *source,
            ));
        }
        list
    }
}

/// How many lines a script may have and still show in full at a gate.
pub const DEFAULT_SCRIPT_LINES: usize = 20;

/// The config file's path: `KORIN_CONFIG`, else under `XDG_CONFIG_HOME`, else under `~/.config`.
pub fn path() -> PathBuf {
    if let Some(path) = std::env::var_os("KORIN_CONFIG").filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("korin").join("config.kdl")
}

/// Loads the config file at [`path`].
///
/// # Errors
///
/// Diagnostics when the file cannot be read or holds a bad key; see [`parse`].
pub fn load() -> Result<Config, Vec<Diag>> {
    load_from(&path())
}

/// Loads the config file at `path`. A missing file gives an empty config.
///
/// # Errors
///
/// Diagnostics when the file cannot be read or holds a bad key; see [`parse`].
pub fn load_from(path: &Path) -> Result<Config, Vec<Diag>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config {
                path: path.to_path_buf(),
                ..Config::default()
            });
        }
        Err(e) => {
            let file = SourceFile::new(path, path.display().to_string(), "");
            return Err(vec![Diag::error(
                &file,
                "config",
                Span::new(0, 0),
                format!("cannot read the config file: {e}"),
            )]);
        }
    };
    parse(path, &text)
}

/// Parses the text of a config file.
///
/// # Errors
///
/// One diagnostic per problem: text that is not KDL, an unknown or repeated key, or a value
/// of the wrong kind.
pub fn parse(path: &Path, text: &str) -> Result<Config, Vec<Diag>> {
    let file = SourceFile::new(path, path.display().to_string(), text);
    let doc = KdlDocument::parse_v2(text).map_err(|e| {
        e.diagnostics
            .iter()
            .map(|d| {
                let mut diag = Diag::error(
                    &file,
                    "config",
                    span(d.span),
                    d.message
                        .clone()
                        .unwrap_or_else(|| "the config file is not valid KDL".into()),
                )
                .label(d.label.clone().unwrap_or_default());
                if let Some(help) = &d.help {
                    diag = diag.help(help.clone());
                }
                diag
            })
            .collect::<Vec<_>>()
    })?;
    let mut config = Config {
        path: path.to_path_buf(),
        exists: true,
        ..Config::default()
    };
    let mut diags = Vec::new();
    let mut seen: Vec<(&str, Span)> = Vec::new();
    for node in doc.nodes() {
        let name = node.name().value();
        let at = span(node.span());
        let Some(key) = KEYS.iter().find(|k| **k == name) else {
            diags.push(
                Diag::error(&file, "config", at, format!("unknown config key `{name}`"))
                    .label("not a key")
                    .help(format!("the keys are {}", list(KEYS))),
            );
            continue;
        };
        if let Some((_, first)) = seen.iter().find(|(k, _)| k == key) {
            diags.push(
                Diag::error(
                    &file,
                    "config",
                    at,
                    format!("`{key}` appears more than once"),
                )
                .label("again here")
                .related(*first, "first set here"),
            );
            continue;
        }
        seen.push((key, at));
        let result = match *key {
            "view" => one_of(node, &["compact", "full"]).map(|v| {
                config.view = Some(if v == "full" {
                    View::Full
                } else {
                    View::Compact
                });
            }),
            "legend" => boolean(node).map(|b| config.legend = Some(b)),
            "script-lines" => count(node).map(|n| config.script_lines = Some(n)),
            "pager" => command_text(node).map(|t| config.pager = Some(t)),
            "require-enter" => boolean(node).map(|b| config.require_enter = Some(b)),
            "color" => one_of(node, &["auto", "always", "never"]).map(|v| {
                config.color = Some(match v.as_str() {
                    "always" => Choice::Always,
                    "never" => Choice::Never,
                    _ => Choice::Auto,
                });
            }),
            "open" => open_block(node, &file, &mut diags).map(|kinds| config.open = kinds),
            "icons" => one_of(node, &["plain", "nerd"]).map(|v| {
                config.icons = Some(if v == "nerd" {
                    Icons::Nerd
                } else {
                    Icons::Plain
                });
            }),
            _ => unreachable!("every key in KEYS has an arm"),
        };
        if let Err((message, help)) = result {
            diags.push(
                Diag::error(&file, "config", at, format!("`{key}` {message}"))
                    .label("here")
                    .help(help),
            );
        }
    }
    if diags.is_empty() {
        Ok(config)
    } else {
        Err(diags)
    }
}

/// Merges the flags, the environment and the file into each key's effective value.
pub fn resolve(config: &Config, flags: Flags) -> Effective {
    let view = config
        .view
        .map_or((View::Compact, Source::Default), |v| (v, Source::File));
    let legend = config
        .legend
        .map_or((true, Source::Default), |v| (v, Source::File));
    let script_lines = if let Some(n) = flags.script_lines {
        (n, Source::Flag)
    } else if let Some(n) = std::env::var("KORIN_SCRIPT_LINES")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
    {
        (n, Source::Env)
    } else {
        config
            .script_lines
            .map_or((DEFAULT_SCRIPT_LINES, Source::Default), |n| {
                (n, Source::File)
            })
    };
    let pager = match &config.pager {
        Some(p) => (Some(p.clone()), Source::File),
        None => (None, Source::Default),
    };
    let require_enter = if flags.require_enter {
        (true, Source::Flag)
    } else if let Some(v) = std::env::var_os("KORIN_REQUIRE_ENTER").filter(|v| !v.is_empty()) {
        (v != "0", Source::Env)
    } else {
        config
            .require_enter
            .map_or((false, Source::Default), |v| (v, Source::File))
    };
    let color = if let Some(c) = flags.color {
        (c, Source::Flag)
    } else {
        config
            .color
            .map_or((Choice::Auto, Source::Default), |c| (c, Source::File))
    };
    let icons = config
        .icons
        .map_or((Icons::Plain, Source::Default), |i| (i, Source::File));
    let open = OPEN_KINDS
        .iter()
        .map(|kind| {
            let env = format!("KORIN_OPEN_{}", kind.to_uppercase());
            if let Some(command) = std::env::var(&env).ok().filter(|c| !c.trim().is_empty()) {
                (*kind, Some(command), Source::Env)
            } else if let Some((_, command)) = config.open.iter().find(|(k, _)| k == kind) {
                (*kind, Some(command.clone()), Source::File)
            } else {
                (*kind, None, Source::Default)
            }
        })
        .collect();
    Effective {
        view,
        legend,
        script_lines,
        pager,
        require_enter,
        color,
        icons,
        open,
    }
}

/// The node's single argument, which must be a string.
fn argument(node: &KdlNode) -> Result<&KdlValue, (String, String)> {
    let entries = node.entries();
    if node.children().is_some() {
        return Err(("takes no children".into(), "write it on one line".into()));
    }
    match entries {
        [entry] if entry.name().is_none() => Ok(entry.value()),
        _ => Err((
            "takes exactly one value".into(),
            format!("write it as `{} <value>`", node.name().value()),
        )),
    }
}

fn one_of(node: &KdlNode, values: &[&str]) -> Result<String, (String, String)> {
    match argument(node)? {
        KdlValue::String(s) if values.contains(&s.as_str()) => Ok(s.clone()),
        other => Err((
            format!("cannot be {other}"),
            format!("use one of {}", list(values)),
        )),
    }
}

fn count(node: &KdlNode) -> Result<usize, (String, String)> {
    match argument(node)? {
        KdlValue::Integer(n) if *n >= 0 => usize::try_from(*n).map_err(|_| {
            (
                "is too large".to_string(),
                "use a smaller number".to_string(),
            )
        }),
        other => Err((
            format!("cannot be {other}"),
            "use a whole number, 0 or more".into(),
        )),
    }
}

/// An `open` block: no value of its own, and one child per kind, each with a command.
/// Problems with a child go into `diags` with the child's own span.
fn open_block(
    node: &KdlNode,
    file: &std::sync::Arc<SourceFile>,
    diags: &mut Vec<Diag>,
) -> Result<Vec<(&'static str, String)>, (String, String)> {
    if !node.entries().is_empty() {
        return Err((
            "takes no value".into(),
            "put each opener on its own line inside braces, such as `open { json \"jnv\" }`".into(),
        ));
    }
    let Some(children) = node.children() else {
        return Err((
            "needs a block".into(),
            "write `open { json \"jnv\" }`".into(),
        ));
    };
    let mut kinds: Vec<(&'static str, String)> = Vec::new();
    for child in children.nodes() {
        let name = child.name().value();
        let at = span(child.span());
        let Some(kind) = OPEN_KINDS.iter().find(|k| **k == name) else {
            diags.push(
                Diag::error(
                    file,
                    "config",
                    at,
                    format!("unknown kind `{name}` in `open`"),
                )
                .label("not a kind")
                .help(format!("the kinds are {}", list(OPEN_KINDS))),
            );
            continue;
        };
        if kinds.iter().any(|(k, _)| k == kind) {
            diags.push(
                Diag::error(
                    file,
                    "config",
                    at,
                    format!("`open {kind}` appears more than once"),
                )
                .label("again here"),
            );
            continue;
        }
        match command_text(child) {
            Ok(command) => kinds.push((kind, command)),
            Err((message, help)) => diags.push(
                Diag::error(file, "config", at, format!("`open {kind}` {message}"))
                    .label("here")
                    .help(help),
            ),
        }
    }
    Ok(kinds)
}

fn command_text(node: &KdlNode) -> Result<String, (String, String)> {
    match argument(node)? {
        KdlValue::String(s) if !s.trim().is_empty() => Ok(s.clone()),
        other => Err((
            format!("cannot be {other}"),
            "use a command in quotes, such as \"less -R\"".into(),
        )),
    }
}

fn boolean(node: &KdlNode) -> Result<bool, (String, String)> {
    match argument(node)? {
        KdlValue::Bool(b) => Ok(*b),
        other => Err((
            format!("cannot be {other}"),
            "use `#true` or `#false`".into(),
        )),
    }
}

fn list(values: &[&str]) -> String {
    values
        .iter()
        .map(|v| format!("`{v}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn span(s: miette::SourceSpan) -> Span {
    Span::new(s.offset(), s.offset() + s.len())
}

fn view_name(view: View) -> &'static str {
    match view {
        View::Compact => "compact",
        View::Full => "full",
    }
}

fn bool_name(b: bool) -> &'static str {
    if b { "#true" } else { "#false" }
}

fn choice_name(choice: Choice) -> &'static str {
    match choice {
        Choice::Auto => "auto",
        Choice::Always => "always",
        Choice::Never => "never",
    }
}
