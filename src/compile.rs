//! Loads a wizard and its modules, expands template calls, resolves names, and checks every
//! rule that can be checked without running anything.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::ast::{self, CaptureMode, CommandKind, Span};
use crate::diag::{Diag, SourceFile};
use crate::ir::{self, IPart, Loc, Sym, SymInfo, SymKind, Ty};
use crate::jq::Jq;
use crate::parser;
use crate::stdlib;

/// The result of compiling a wizard: the program and any warnings, or every diagnostic.
pub type Compiled = Result<(ir::Program, Vec<Diag>), Vec<Diag>>;

/// Compiles the wizard at `path`.
///
/// # Errors
///
/// Returns every error and warning if any error was found.
pub fn compile_path(path: &Path) -> Compiled {
    let name = path.display().to_string();
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            compile(&SourceFile::new(resolved, name, text))
        }
        Err(error) => {
            let file = SourceFile::new(path, name, "");
            Err(vec![Diag::error(
                &file,
                "io",
                Span::default(),
                format!("cannot read {}: {error}", path.display()),
            )])
        }
    }
}

/// Compiles a wizard that is already loaded.
///
/// # Errors
///
/// Returns every error and warning if any error was found.
pub fn compile(file: &Arc<SourceFile>) -> Compiled {
    let wizard = parser::parse_wizard(file)?;
    let mut compiler = Compiler::new(file, &wizard);
    let program = compiler.wizard(file, &wizard);
    let has_error = compiler.diags.iter().any(Diag::is_error);
    if has_error {
        Err(compiler.diags)
    } else {
        Ok((program, compiler.diags))
    }
}

type ModId = usize;

struct LoadedModule {
    file: Arc<SourceFile>,
    key: String,
    defines: HashMap<String, Arc<ast::Define>>,
    imports: HashMap<String, ModId>,
}

#[derive(Clone)]
enum Binding {
    Sym(Sym),
    /// A template parameter, bound to the caller's argument. `list` is set when the argument
    /// is a whole list.
    Arg {
        text: ir::Interpolated,
        list: Option<Sym>,
    },
    Alias,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Guard {
    Not(Sym),
    Is(Sym),
    Exists(Sym),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ICtx {
    /// A command argument.
    Argv,
    /// A template call argument.
    Arg,
    /// A message or prompt shown to the human.
    Text,
    Env,
    Path,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ETy {
    Bool,
    Value,
    List,
}

struct Naming {
    alias: String,
    exports: HashSet<String>,
}

struct Ctx {
    imports: HashMap<String, ModId>,
    file: Arc<SourceFile>,
    scopes: Vec<HashMap<String, Binding>>,
    guards: Vec<Guard>,
    stage: usize,
    naming: Option<Naming>,
}

impl Ctx {
    fn loc(&self, span: Span) -> Loc {
        Loc {
            file: Arc::clone(&self.file),
            span,
        }
    }

    fn lookup(&self, name: &str) -> Option<&Binding> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name))
    }

    fn has_guard(&self, fact: Guard) -> bool {
        self.guards.contains(&fact)
    }
}

struct Allow {
    names: HashSet<String>,
    paths: HashSet<PathBuf>,
    span: Span,
}

struct Compiler {
    wizard_file: Arc<SourceFile>,
    mods: Vec<LoadedModule>,
    by_key: HashMap<String, ModId>,
    loading: Vec<String>,
    syms: Vec<SymInfo>,
    diags: Vec<Diag>,
    references: Vec<ir::Reference>,
    call_count: usize,
    call_stack: Vec<String>,
    allow: Option<Allow>,
    unattended: Option<Span>,
    korin_out: Sym,
}

impl Compiler {
    fn new(file: &Arc<SourceFile>, wizard: &ast::Wizard) -> Self {
        let allow = wizard.header.allow.as_ref().map(|entries| {
            let mut names = HashSet::new();
            let mut paths = HashSet::new();
            for entry in entries {
                if entry.value.contains('/') {
                    paths.insert(normalize(&file.dir().join(&entry.value)));
                } else {
                    names.insert(entry.value.clone());
                }
            }
            let span = entries
                .iter()
                .map(|e| e.span)
                .reduce(Span::join)
                .unwrap_or_default();
            Allow { names, paths, span }
        });
        let korin_out = SymInfo {
            name: "korin.out".into(),
            kind: SymKind::KorinOut,
            ty: Ty::Scalar,
            secret: false,
            optional: false,
            stage: 0,
            producer: None,
            step_expects: Vec::new(),
            on: None,
            from_secret_step: false,
            loc: Loc {
                file: Arc::clone(file),
                span: Span::default(),
            },
        };
        Self {
            wizard_file: Arc::clone(file),
            mods: Vec::new(),
            by_key: HashMap::new(),
            loading: Vec::new(),
            syms: vec![korin_out],
            diags: Vec::new(),
            references: Vec::new(),
            call_count: 0,
            call_stack: Vec::new(),
            allow,
            unattended: wizard.header.unattended,
            korin_out: 0,
        }
    }

    fn error(
        &mut self,
        file: &Arc<SourceFile>,
        code: &'static str,
        span: Span,
        message: impl Into<String>,
    ) -> &mut Diag {
        self.diags.push(Diag::error(file, code, span, message));
        self.diags.last_mut().expect("just pushed")
    }

    fn push(&mut self, diag: Diag) {
        self.diags.push(diag);
    }

    // ---- modules ----

    fn imports(&mut self, file: &Arc<SourceFile>, uses: &[ast::Use]) -> HashMap<String, ModId> {
        let mut imports = HashMap::new();
        for u in uses {
            let loaded = match &u.target {
                ast::UseTarget::Builtin(name) => {
                    let Some(text) = stdlib::source(&name.value) else {
                        let known = stdlib::NAMES.join(", ");
                        self.push(
                            Diag::error(file, "module", name.span, format!("unknown built-in module `{}`", name.value))
                                .help(format!("built-in modules are: {known}; a file needs a quoted path, such as `use \"./lib/x.korin\"`")),
                        );
                        continue;
                    };
                    let key = format!("builtin:{}", name.value);
                    let source = || {
                        Ok(SourceFile::new(
                            format!("<built-in>/{}.korin", name.value),
                            format!("{} (built-in)", name.value),
                            text,
                        ))
                    };
                    self.load(file, u.span, &key, source)
                        .map(|id| (id, name.value.clone()))
                }
                ast::UseTarget::Path(path) => {
                    let joined = file.dir().join(&path.value);
                    let Ok(canonical) = std::fs::canonicalize(&joined) else {
                        self.push(
                            Diag::error(
                                file,
                                "module",
                                path.span,
                                format!("cannot find module `{}`", path.value),
                            )
                            .label(format!("looked for {}", joined.display()))
                            .help("paths are relative to the file that contains the `use`"),
                        );
                        continue;
                    };
                    let stem = canonical
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let key = canonical.display().to_string();
                    let display = path.value.clone();
                    let source = || {
                        std::fs::read_to_string(&canonical)
                            .map(|text| SourceFile::new(canonical.clone(), display.clone(), text))
                            .map_err(|e| e.to_string())
                    };
                    self.load(file, u.span, &key, source).map(|id| (id, stem))
                }
            };
            let Some((id, default_name)) = loaded else {
                continue;
            };
            let namespace = u.alias.as_ref().map_or(default_name, |a| a.value.clone());
            if imports.insert(namespace.clone(), id).is_some() {
                self.push(
                    Diag::error(
                        file,
                        "module",
                        u.span,
                        format!("the name `{namespace}` is already used by another import"),
                    )
                    .help("rename one import with `as <name>`"),
                );
            }
        }
        imports
    }

    fn load(
        &mut self,
        from: &Arc<SourceFile>,
        span: Span,
        key: &str,
        source: impl FnOnce() -> Result<Arc<SourceFile>, String>,
    ) -> Option<ModId> {
        if let Some(position) = self.loading.iter().position(|k| k == key) {
            let mut chain: Vec<String> = self.loading[position..]
                .iter()
                .map(|k| short_key(k))
                .collect();
            chain.push(short_key(key));
            self.push(
                Diag::error(from, "import-cycle", span, "this import creates a cycle")
                    .label("imported here")
                    .help(format!("import chain: {}", chain.join(" → "))),
            );
            return None;
        }
        if let Some(id) = self.by_key.get(key) {
            return Some(*id);
        }
        let file = match source() {
            Ok(file) => file,
            Err(error) => {
                self.push(Diag::error(
                    from,
                    "module",
                    span,
                    format!("cannot read module: {error}"),
                ));
                return None;
            }
        };
        let module = match parser::parse_module(&file) {
            Ok(module) => module,
            Err(diags) => {
                self.diags.extend(diags);
                return None;
            }
        };
        self.loading.push(key.to_string());
        let imports = self.imports(&file, &module.uses);
        self.loading.pop();
        let mut defines = HashMap::new();
        for define in module.defines {
            let name = define.name.value.clone();
            let span = define.name.span;
            if defines.insert(name.clone(), Arc::new(define)).is_some() {
                self.push(Diag::error(
                    &file,
                    "template",
                    span,
                    format!("template `{name}` is defined twice"),
                ));
            }
        }
        let id = self.mods.len();
        self.mods.push(LoadedModule {
            file,
            key: key.to_string(),
            defines,
            imports,
        });
        self.by_key.insert(key.to_string(), id);
        Some(id)
    }

    // ---- wizard ----

    fn wizard(&mut self, file: &Arc<SourceFile>, wizard: &ast::Wizard) -> ir::Program {
        let imports = self.imports(file, &wizard.uses);
        let mut ctx = Ctx {
            imports,
            file: Arc::clone(file),
            scopes: vec![HashMap::new()],
            guards: Vec::new(),
            stage: 0,
            naming: None,
        };

        let mut params = Vec::new();
        for param in &wizard.params {
            let (required, secret, default) = match &param.kind {
                ast::ParamKind::Required => (true, false, None),
                ast::ParamKind::Secret => (false, true, None),
                ast::ParamKind::Default(template) => (
                    false,
                    false,
                    Some(self.interpolated(&mut ctx, template, ICtx::Text)),
                ),
            };
            let sym = self.new_sym(
                &ctx,
                &param.name.value,
                SymKind::Param,
                Ty::Scalar,
                param.name.span,
            );
            // A default built from a secret is itself secret.
            let derived = default
                .as_ref()
                .is_some_and(|d| d.syms().any(|s| self.syms[s].secret));
            self.syms[sym].secret = secret || derived;
            self.define_name(
                &mut ctx,
                &param.name.value,
                param.name.span,
                Binding::Sym(sym),
            );
            params.push(ir::Param {
                sym,
                required,
                secret,
                default,
            });
        }

        let mut stages = Vec::new();
        let mut titles: HashMap<&str, Span> = HashMap::new();
        for (index, stage) in wizard.stages.iter().enumerate() {
            if let Some(first) = titles.insert(&stage.title.value, stage.title.span) {
                self.push(
                    Diag::error(
                        file,
                        "stage",
                        stage.title.span,
                        format!("two stages are titled \"{}\"", stage.title.value),
                    )
                    .related(first, "first used here")
                    .help("stage titles must be unique, so `--from` can name them"),
                );
            }
            ctx.stage = index + 1;
            let steps = self.steps(&mut ctx, &stage.steps, &(index + 1).to_string());
            if wizard.header.mode == ast::Mode::Guide {
                self.check_guide(&steps, None, wizard.header.mode_span);
            }
            stages.push(ir::Stage {
                number: index + 1,
                title: stage.title.value.clone(),
                mutating: steps_mutating(&steps),
                steps,
                loc: ctx.loc(stage.span),
            });
        }

        ir::Program {
            file: Arc::clone(file),
            title: wizard.header.title.value.clone(),
            gates: wizard.header.gates,
            mode: wizard.header.mode,
            unattended: wizard.header.unattended.is_some(),
            params,
            stages,
            syms: std::mem::take(&mut self.syms),
            references: std::mem::take(&mut self.references),
            korin_out: self.korin_out,
        }
    }

    // ---- names ----

    fn new_sym(&mut self, ctx: &Ctx, local: &str, kind: SymKind, ty: Ty, span: Span) -> Sym {
        let name = match &ctx.naming {
            None => local.to_string(),
            Some(naming) if naming.exports.contains(local) => format!("{}.{local}", naming.alias),
            Some(naming) => format!("{}${local}", naming.alias),
        };
        self.syms.push(SymInfo {
            name,
            kind,
            ty,
            secret: false,
            optional: false,
            stage: ctx.stage,
            producer: None,
            step_expects: Vec::new(),
            on: None,
            from_secret_step: false,
            loc: ctx.loc(span),
        });
        self.syms.len() - 1
    }

    fn define_name(&mut self, ctx: &mut Ctx, name: &str, span: Span, binding: Binding) -> bool {
        if ctx.lookup(name).is_some() || name == "korin" {
            let file = Arc::clone(&ctx.file);
            self.error(&file, "name", span, format!("`{name}` is already defined"))
                .help =
                Some("names are unique within a wizard and cannot be redefined or shadowed".into());
            return false;
        }
        ctx.scopes
            .last_mut()
            .expect("there is always a scope")
            .insert(name.to_string(), binding);
        true
    }

    fn require_available(&mut self, ctx: &Ctx, sym: Sym, span: Span) {
        let info = &self.syms[sym];
        if info.kind != SymKind::Capture {
            return;
        }
        let name = info.name.clone();
        if let Some(on) = info.on {
            if !ctx.has_guard(Guard::Is(on)) {
                let expect = self.syms[on].name.clone();
                let file = Arc::clone(&ctx.file);
                self.error(
                    &file,
                    "guard",
                    span,
                    format!("`{name}` exists only when `expect {expect}` matched"),
                )
                .help = Some(format!("use it inside `when {expect}`"));
                return;
            }
        } else if !info.step_expects.is_empty() {
            let missing: Vec<Sym> = info
                .step_expects
                .iter()
                .copied()
                .filter(|o| !ctx.has_guard(Guard::Not(*o)))
                .collect();
            if !missing.is_empty() {
                let all: Vec<String> = info
                    .step_expects
                    .iter()
                    .map(|o| format!("not {}", self.syms[*o].name))
                    .collect();
                let file = Arc::clone(&ctx.file);
                self.error(
                    &file,
                    "guard",
                    span,
                    format!("`{name}` exists only when no `expect` of its step matched"),
                )
                .help = Some(format!("use it inside `when {}`", all.join(" and ")));
                return;
            }
        }
        if self.syms[sym].optional && !ctx.has_guard(Guard::Exists(sym)) {
            let file = Arc::clone(&ctx.file);
            self.error(
                &file,
                "guard",
                span,
                format!("`{name}` is optional and may be absent"),
            )
            .help = Some(format!("use it inside `when exists {name}`"));
        }
    }

    fn record_reference(&mut self, ctx: &Ctx, sym: Sym, span: Span) {
        let info = &self.syms[sym];
        if matches!(info.kind, SymKind::Param | SymKind::KorinOut) || info.stage == 0 {
            return;
        }
        self.references.push(ir::Reference {
            stage: ctx.stage,
            sym,
            loc: ctx.loc(span),
        });
    }

    // ---- templates ----

    fn interpolated(
        &mut self,
        ctx: &mut Ctx,
        template: &ast::Interpolated,
        kind: ICtx,
    ) -> ir::Interpolated {
        let whole = template.parts.len() == 1;
        let mut parts = Vec::new();
        for part in &template.parts {
            match part {
                ast::Part::Lit(text) => parts.push(IPart::Lit(text.clone())),
                ast::Part::Ref(r) => parts.extend(self.interpolated_ref(ctx, r, kind, whole)),
            }
        }
        ir::Interpolated {
            parts,
            loc: ctx.loc(template.span),
        }
    }

    fn interpolated_ref(
        &mut self,
        ctx: &mut Ctx,
        r: &ast::Ref,
        kind: ICtx,
        whole: bool,
    ) -> Vec<IPart> {
        let file = Arc::clone(&ctx.file);
        let dotted = r.dotted();
        if dotted == "korin.out" {
            if kind == ICtx::Text {
                self.error(
                    &file,
                    "name",
                    r.span,
                    "`{korin.out}` can be used only in a command, an `env` value or a file path",
                );
            } else if r.splat {
                self.error(&file, "type", r.span, "`korin.out` is not a list");
            }
            return vec![IPart::Sym {
                sym: self.korin_out,
                splat: false,
                loc: ctx.loc(r.span),
            }];
        }
        match ctx.lookup(&dotted).cloned() {
            None => {
                self.unknown_name(ctx, r);
                Vec::new()
            }
            Some(Binding::Alias) => {
                self.error(
                    &file,
                    "name",
                    r.span,
                    format!("`{dotted}` is a template alias, not a value"),
                )
                .help = Some(format!(
                    "use one of its exports, such as `{{{dotted}.<name>}}`"
                ));
                Vec::new()
            }
            Some(Binding::Arg {
                text: template,
                list,
            }) => match (list, r.splat) {
                (Some(sym), true) => {
                    if !(whole && matches!(kind, ICtx::Argv | ICtx::Arg)) {
                        self.error(
                            &file,
                            "type",
                            r.span,
                            "`{list...}` must be a whole word in a command",
                        );
                    }
                    vec![IPart::Sym {
                        sym,
                        splat: true,
                        loc: ctx.loc(r.span),
                    }]
                }
                (Some(sym), false) if kind == ICtx::Arg && whole => vec![IPart::Sym {
                    sym,
                    splat: false,
                    loc: ctx.loc(r.span),
                }],
                (Some(_), false) => {
                    self.error(&file, "type", r.span, format!("`{dotted}` is a list"))
                        .help = Some(format!(
                        "write `{{{dotted}...}}` to pass one argument per item"
                    ));
                    Vec::new()
                }
                (None, true) => {
                    self.error(&file, "type", r.span, format!("`{dotted}` is not a list"));
                    Vec::new()
                }
                (None, false) => {
                    let has_out = template
                        .parts
                        .iter()
                        .any(|p| matches!(p, IPart::Sym { sym, .. } if *sym == self.korin_out));
                    if kind == ICtx::Text && has_out {
                        self.error(
                            &file,
                            "name",
                            r.span,
                            format!("`{dotted}` holds `{{korin.out}}`, which can be used only in a command, an `env` value or a file path"),
                        );
                    }
                    template.parts
                }
            },
            Some(Binding::Sym(sym)) => {
                self.check_interpolation(ctx, sym, r, kind, whole);
                self.require_available(ctx, sym, r.span);
                self.record_reference(ctx, sym, r.span);
                vec![IPart::Sym {
                    sym,
                    splat: r.splat,
                    loc: ctx.loc(r.span),
                }]
            }
        }
    }

    fn check_interpolation(&mut self, ctx: &Ctx, sym: Sym, r: &ast::Ref, kind: ICtx, whole: bool) {
        let file = Arc::clone(&ctx.file);
        let info = &self.syms[sym];
        let name = info.name.clone();
        match info.ty {
            Ty::Bool => {
                self.error(
                    &file,
                    "type",
                    r.span,
                    format!("`{name}` is an `expect`, so it cannot be interpolated"),
                )
                .help = Some(format!("use it in a condition, such as `when {name}`"));
            }
            Ty::List => {
                let command_word = whole && matches!(kind, ICtx::Argv | ICtx::Arg);
                if !matches!(kind, ICtx::Argv | ICtx::Arg) {
                    self.error(
                        &file,
                        "type",
                        r.span,
                        format!("`{name}` is a list, which cannot be interpolated into a string"),
                    );
                } else if r.splat && !command_word {
                    self.error(
                        &file,
                        "type",
                        r.span,
                        "`{list...}` must be a whole word in a command",
                    );
                } else if !r.splat && (kind != ICtx::Arg || !whole) {
                    self.error(&file, "type", r.span, format!("`{name}` is a list"))
                        .help = Some(format!(
                        "write `{{{name}...}}` to pass one argument per item"
                    ));
                }
            }
            Ty::Scalar | Ty::Json => {
                if r.splat {
                    self.error(
                        &file,
                        "type",
                        r.span,
                        format!("`{name}` is not a list, so it cannot be splatted"),
                    );
                }
            }
        }
    }

    fn unknown_name(&mut self, ctx: &Ctx, r: &ast::Ref) {
        let file = Arc::clone(&ctx.file);
        let dotted = r.dotted();
        let help = if r.path.len() > 1 && matches!(ctx.lookup(&r.path[0]), Some(Binding::Alias)) {
            format!(
                "the template behind `{}` does not export `{}`",
                r.path[0],
                r.path[1..].join(".")
            )
        } else if r.path.len() > 1 {
            "captures have no fields; dotted names exist only for template exports".into()
        } else {
            "define it with `param`, `ask` or `capture` before this line".into()
        };
        self.error(&file, "name", r.span, format!("unknown name `{dotted}`"))
            .help = Some(help);
    }

    fn check_no_secret(&mut self, template: &ir::Interpolated, what: &str) {
        for part in &template.parts {
            if let IPart::Sym { sym, loc, .. } = part
                && self.syms[*sym].secret
            {
                let name = self.syms[*sym].name.clone();
                self.push(
                    Diag::error(
                        &loc.file,
                        "secret",
                        loc.span,
                        format!("secret `{name}` cannot appear in {what}"),
                    )
                    .help(
                        "korin never shows secrets; pass them to commands with `env NAME={secret}`",
                    ),
                );
            }
        }
    }

    /// Whether a path names a file inside `{korin.out}`: exactly `{korin.out}` followed by one
    /// literal `/name...` with no `..`. Anything else, such as `{korin.out}-x` or a `{value}`
    /// that could hold `../` at run time, could reach outside the scratch directory.
    fn inside_korin_out(&self, path: &ir::Interpolated) -> bool {
        match path.parts.as_slice() {
            [IPart::Sym { sym, .. }, IPart::Lit(rest)] if *sym == self.korin_out => {
                rest.starts_with('/')
                    && rest.split('/').any(|c| !c.is_empty())
                    && !rest.split('/').any(|c| c == "..")
            }
            _ => false,
        }
    }

    /// A poll repeats reads until a condition holds. Anything that changes state, asks the
    /// human or takes the terminal would repeat with it, so the checker rejects it.
    fn check_poll_body(&mut self, body: &[ir::Step]) {
        for step in body {
            let what = match &step.kind {
                ir::StepKind::Command(c) if c.kind.is_mutating() => Some(c.kind.keyword()),
                ir::StepKind::Confirm(_) => Some("confirm"),
                ir::StepKind::Ask { .. } => Some("ask"),
                ir::StepKind::When { body, .. }
                | ir::StepKind::Each { body, .. }
                | ir::StepKind::Poll { body, .. }
                | ir::StepKind::Call { body, .. } => {
                    self.check_poll_body(body);
                    None
                }
                _ => None,
            };
            if let Some(what) = what {
                self.push(
                    Diag::error(
                        &step.loc.file,
                        "poll",
                        step.loc.span,
                        format!("a `poll` repeats reads only, so it cannot contain `{what}`"),
                    )
                    .help("move the step before or after the poll"),
                );
            }
        }
    }

    /// A guide wizard only reads, so the checker rejects every step that changes state, at
    /// any depth. A step inside a template is reported at the call in the wizard, which is
    /// where the author can act; `call` is that call's step, once the walk is inside one.
    fn check_guide(&mut self, steps: &[ir::Step], call: Option<&ir::Step>, mode: Option<Span>) {
        for step in steps {
            match &step.kind {
                ir::StepKind::Command(c)
                    if matches!(c.kind, CommandKind::Mutate | CommandKind::ScriptMutate) =>
                {
                    let keyword = c.kind.keyword();
                    let mut diag = match call {
                        Some(call) => {
                            let label = match &call.kind {
                                ir::StepKind::Call { label, .. } => label.as_str(),
                                _ => "the template",
                            };
                            let (line, _) = step.loc.file.line_col(step.loc.span.start);
                            Diag::error(
                                &call.loc.file,
                                "guide",
                                call.loc.span,
                                format!("`{label}` runs `{keyword}`, and a guide only reads"),
                            )
                            .label("this template call")
                            .help(format!(
                                "step {} at {}:{line} is `{keyword}`; call a template that only \
                                 reads, or remove `mode guide`",
                                step.id, step.loc.file.name
                            ))
                        }
                        None => Diag::error(
                            &step.loc.file,
                            "guide",
                            step.loc.span,
                            format!("a guide only reads, so it cannot contain `{keyword}`"),
                        )
                        .help("use `read` or `script read`, or remove `mode guide` to gate each stage"),
                    };
                    let in_wizard = Arc::ptr_eq(&diag_file(call, step), &self.wizard_file);
                    if let Some(mode) = mode
                        && in_wizard
                    {
                        diag = diag.related(mode, "guide mode declared here");
                    }
                    self.push(diag);
                }
                ir::StepKind::Call { body, .. } => {
                    self.check_guide(body, call.or(Some(step)), mode);
                }
                ir::StepKind::When { body, .. }
                | ir::StepKind::Each { body, .. }
                | ir::StepKind::Poll { body, .. } => self.check_guide(body, call, mode),
                _ => {}
            }
        }
    }

    // ---- expressions ----

    fn condition(&mut self, ctx: &mut Ctx, expr: &ast::Expr) -> ir::Expr {
        let (lowered, ty) = self.expr(ctx, expr);
        if ty != ETy::Bool {
            let file = Arc::clone(&ctx.file);
            self.error(
                &file,
                "type",
                expr.span(),
                "a condition must be true or false",
            )
            .help = Some("use an `expect`, a comparison, `in`, `~` or `exists`".into());
        }
        lowered
    }

    fn expr(&mut self, ctx: &mut Ctx, expr: &ast::Expr) -> (ir::Expr, ETy) {
        let file = Arc::clone(&ctx.file);
        let poison = (ir::Expr::Num("0".into()), ETy::Bool);
        match expr {
            ast::Expr::Ref(r) => {
                let dotted = r.dotted();
                match ctx.lookup(&dotted).cloned() {
                    None => {
                        self.unknown_name(ctx, r);
                        poison
                    }
                    Some(Binding::Alias) => {
                        self.error(
                            &file,
                            "name",
                            r.span,
                            format!("`{dotted}` is a template alias, not a value"),
                        );
                        poison
                    }
                    Some(Binding::Arg {
                        text: template,
                        list,
                    }) => match list {
                        Some(sym) => (ir::Expr::Sym(sym), ETy::List),
                        None => (ir::Expr::Str(template), ETy::Value),
                    },
                    Some(Binding::Sym(sym)) => {
                        let ty = self.syms[sym].ty;
                        if ty != Ty::Bool {
                            self.require_available(ctx, sym, r.span);
                        }
                        self.record_reference(ctx, sym, r.span);
                        let ety = match ty {
                            Ty::Bool => ETy::Bool,
                            Ty::List => ETy::List,
                            Ty::Scalar | Ty::Json => ETy::Value,
                        };
                        (ir::Expr::Sym(sym), ety)
                    }
                }
            }
            ast::Expr::Exists(r) => {
                let dotted = r.dotted();
                match ctx.lookup(&dotted).cloned() {
                    Some(Binding::Sym(sym)) if self.syms[sym].optional => {
                        self.record_reference(ctx, sym, r.span);
                        (ir::Expr::Exists(sym), ETy::Bool)
                    }
                    Some(_) => {
                        self.error(&file, "type", r.span, format!("`exists` applies only to an optional capture, and `{dotted}` is not one"))
                            .help = Some("mark a capture optional with `?`, as in `capture name? = ...`".into());
                        poison
                    }
                    None => {
                        self.unknown_name(ctx, r);
                        poison
                    }
                }
            }
            ast::Expr::Str(template) => (
                ir::Expr::Str(self.interpolated(ctx, template, ICtx::Text)),
                ETy::Value,
            ),
            ast::Expr::Num(n) => (ir::Expr::Num(n.value.clone()), ETy::Value),
            ast::Expr::List(items, _) => {
                let mut lowered = Vec::new();
                for item in items {
                    let (e, ty) = self.expr(ctx, item);
                    if ty != ETy::Value {
                        self.error(
                            &file,
                            "type",
                            item.span(),
                            "list items must be strings, numbers or names of scalars",
                        );
                    }
                    lowered.push(e);
                }
                (ir::Expr::List(lowered), ETy::List)
            }
            ast::Expr::Cmp(op, a, b) => {
                let (la, ta) = self.expr(ctx, a);
                let (lb, tb) = self.expr(ctx, b);
                for (ty, e) in [(ta, a), (tb, b)] {
                    if ty != ETy::Value {
                        self.error(
                            &file,
                            "type",
                            e.span(),
                            "comparisons take values, not `expect` names or lists",
                        );
                    }
                }
                (ir::Expr::Cmp(*op, Box::new(la), Box::new(lb)), ETy::Bool)
            }
            ast::Expr::In(a, b) => {
                let (la, ta) = self.expr(ctx, a);
                let (lb, tb) = self.expr(ctx, b);
                if ta != ETy::Value {
                    self.error(
                        &file,
                        "type",
                        a.span(),
                        "the left side of `in` must be a value",
                    );
                }
                if tb != ETy::List {
                    self.error(
                        &file,
                        "type",
                        b.span(),
                        "the right side of `in` must be a list",
                    );
                }
                (ir::Expr::In(Box::new(la), Box::new(lb)), ETy::Bool)
            }
            ast::Expr::Regex(a, re) => {
                let (la, ta) = self.expr(ctx, a);
                if ta != ETy::Value {
                    self.error(
                        &file,
                        "type",
                        a.span(),
                        "the left side of `~` must be a value",
                    );
                }
                self.check_regex(&file, &re.value, re.span, false);
                (ir::Expr::Regex(Box::new(la), re.value.clone()), ETy::Bool)
            }
            ast::Expr::Not(inner, _) => {
                let lowered = self.condition(ctx, inner);
                (ir::Expr::Not(Box::new(lowered)), ETy::Bool)
            }
            ast::Expr::And(a, b) => {
                let la = self.condition(ctx, a);
                let guards = self.guards_of(&la);
                let depth = ctx.guards.len();
                ctx.guards.extend(guards);
                let lb = self.condition(ctx, b);
                ctx.guards.truncate(depth);
                (ir::Expr::And(Box::new(la), Box::new(lb)), ETy::Bool)
            }
            ast::Expr::Or(a, b) => {
                let la = self.condition(ctx, a);
                let lb = self.condition(ctx, b);
                (ir::Expr::Or(Box::new(la), Box::new(lb)), ETy::Bool)
            }
        }
    }

    /// The guards a condition proves when it is true: its top-level conjuncts.
    fn guards_of(&self, expr: &ir::Expr) -> Vec<Guard> {
        match expr {
            ir::Expr::And(a, b) => {
                let mut guards = self.guards_of(a);
                guards.extend(self.guards_of(b));
                guards
            }
            ir::Expr::Not(inner) => match inner.as_ref() {
                ir::Expr::Sym(sym) if self.syms[*sym].kind == SymKind::Expect => {
                    vec![Guard::Not(*sym)]
                }
                _ => Vec::new(),
            },
            ir::Expr::Sym(sym) if self.syms[*sym].kind == SymKind::Expect => vec![Guard::Is(*sym)],
            ir::Expr::Exists(sym) => vec![Guard::Exists(*sym)],
            _ => Vec::new(),
        }
    }

    fn check_regex(
        &mut self,
        file: &Arc<SourceFile>,
        pattern: &str,
        span: Span,
        needs_group: bool,
    ) {
        match regex::Regex::new(pattern) {
            Err(error) => {
                let text = error.to_string();
                let reason = text
                    .lines()
                    .find_map(|line| line.strip_prefix("error: "))
                    .unwrap_or(&text)
                    .to_string();
                self.push(Diag::error(
                    file,
                    "regex",
                    span,
                    format!("invalid regex: {reason}"),
                ));
            }
            Ok(re) if needs_group && re.captures_len() < 2 => {
                self.push(
                    Diag::error(file, "regex", span, "a `regex` capture needs a group")
                        .help("wrap the part to capture in parentheses, as in `/(\\d+) rows/`"),
                );
            }
            Ok(_) => {}
        }
    }

    // ---- steps ----

    fn steps(&mut self, ctx: &mut Ctx, steps: &[ast::Step], prefix: &str) -> Vec<ir::Step> {
        steps
            .iter()
            .enumerate()
            .filter_map(|(index, step)| self.step(ctx, step, format!("{prefix}.{}", index + 1)))
            .collect()
    }

    fn unattended_error(&mut self, ctx: &Ctx, span: Span, what: &str) {
        if let Some(header) = self.unattended {
            let file = Arc::clone(&ctx.file);
            let wizard = Arc::clone(&self.wizard_file);
            let mut diag = Diag::error(
                &file,
                "unattended",
                span,
                format!("{what} cannot run unattended"),
            )
            .help("remove `unattended allowed`, or remove this step");
            if Arc::ptr_eq(&file, &wizard) {
                diag = diag.related(header, "declared here");
            }
            self.push(diag);
        }
    }

    fn step(&mut self, ctx: &mut Ctx, step: &ast::Step, id: String) -> Option<ir::Step> {
        let loc = ctx.loc(step.span());
        let kind = match step {
            ast::Step::Command(command) => ir::StepKind::Command(self.command(ctx, command, &id)),
            ast::Step::Confirm { text, span } => {
                self.unattended_error(ctx, *span, "`confirm`");
                let text = self.interpolated(ctx, text, ICtx::Text);
                self.check_no_secret(&text, "a `confirm` message");
                ir::StepKind::Confirm(text)
            }
            ast::Step::Ask {
                name,
                secret,
                prompt,
                ..
            } => {
                let prompt = self.interpolated(ctx, prompt, ICtx::Text);
                self.check_no_secret(&prompt, "a prompt");
                let sym = self.new_sym(ctx, &name.value, SymKind::Ask, Ty::Scalar, name.span);
                self.syms[sym].secret = *secret;
                self.syms[sym].producer = Some(id.clone());
                self.define_name(ctx, &name.value, name.span, Binding::Sym(sym));
                ir::StepKind::Ask {
                    sym,
                    secret: *secret,
                    prompt,
                }
            }
            ast::Step::Assert { cond, message, .. } => {
                let cond = self.condition(ctx, cond);
                let message = self.interpolated(ctx, message, ICtx::Text);
                self.check_no_secret(&message, "an `assert` message");
                ir::StepKind::Assert { cond, message }
            }
            ast::Step::When { cond, body, .. } => {
                let cond = self.condition(ctx, cond);
                let guards = self.guards_of(&cond);
                let depth = ctx.guards.len();
                ctx.guards.extend(guards);
                ctx.scopes.push(HashMap::new());
                let body = self.steps(ctx, body, &id);
                ctx.scopes.pop();
                ctx.guards.truncate(depth);
                ir::StepKind::When { cond, body }
            }
            ast::Step::Each {
                var, list, body, ..
            } => {
                let (list_expr, ty) = self.expr(ctx, list);
                if ty != ETy::List {
                    let file = Arc::clone(&ctx.file);
                    self.error(&file, "type", list.span(), "`each` needs a list")
                        .help =
                        Some("use a list literal, a `lines` capture or a `jq list` capture".into());
                }
                let secret = expr_syms(&list_expr).iter().any(|s| self.syms[*s].secret);
                ctx.scopes.push(HashMap::new());
                let sym = self.new_sym(ctx, &var.value, SymKind::EachVar, Ty::Scalar, var.span);
                self.syms[sym].secret = secret;
                // An item of a list that may be withheld may be withheld too.
                self.syms[sym].from_secret_step = expr_syms(&list_expr)
                    .iter()
                    .any(|s| self.syms[*s].from_secret_step);
                self.syms[sym].producer = Some(id.clone());
                self.define_name(ctx, &var.value, var.span, Binding::Sym(sym));
                let body = self.steps(ctx, body, &id);
                ctx.scopes.pop();
                ir::StepKind::Each {
                    var: sym,
                    list: list_expr,
                    body,
                }
            }
            ast::Step::Poll(poll) => {
                if poll.every.value.0 > poll.limit.value.0 {
                    let file = Arc::clone(&ctx.file);
                    self.error(
                        &file,
                        "poll",
                        poll.every.span,
                        "the poll interval is longer than its limit",
                    );
                }
                let body = self.steps(ctx, &poll.body, &id);
                self.check_poll_body(&body);
                let until = self.condition(ctx, &poll.until);
                let fail = poll.fail.as_ref().map(|f| self.condition(ctx, f));
                ir::StepKind::Poll {
                    every: poll.every.value.0,
                    limit: poll.limit.value.0,
                    body,
                    until,
                    fail,
                }
            }
            ast::Step::Say { kind, text, .. } => {
                let text = self.interpolated(ctx, text, ICtx::Text);
                let what = match kind {
                    ast::SayKind::Note => "a `note`",
                    ast::SayKind::Warn => "a `warn`",
                    ast::SayKind::Record => "a `record`",
                };
                self.check_no_secret(&text, what);
                ir::StepKind::Say { kind: *kind, text }
            }
            ast::Step::Call(call) => return self.call(ctx, call, id, loc),
        };
        Some(ir::Step { id, loc, kind })
    }

    #[allow(clippy::too_many_lines)]
    fn command(&mut self, ctx: &mut Ctx, command: &ast::Command, id: &str) -> ir::Command {
        let file = Arc::clone(&ctx.file);
        let argv: Vec<ir::Interpolated> = command
            .argv
            .iter()
            .map(|word| self.interpolated(ctx, word, ICtx::Argv))
            .collect();

        let exe = match argv[0].literal() {
            Some(name) if name.contains('/') => ir::Exe::Path(normalize(&file.dir().join(&name))),
            Some(name) => ir::Exe::Name(name),
            None => {
                self.error(&file, "exe", command.argv[0].span, "the executable must be literal text")
                    .help = Some("korin resolves every executable before stage 1, so it cannot come from a param, an `ask` or a capture".into());
                ir::Exe::Name(String::new())
            }
        };
        if let Some(allow) = &self.allow {
            let allowed = match &exe {
                ir::Exe::Name(name) => name.is_empty() || allow.names.contains(name),
                ir::Exe::Path(path) => allow.paths.contains(path),
            };
            if !allowed {
                let span = allow.span;
                let wizard = Arc::clone(&self.wizard_file);
                let mut diag = Diag::error(
                    &file,
                    "allow",
                    command.argv[0].span,
                    format!("`{}` is not in the `allow` list", exe.display()),
                )
                .help("add it to the `allow` header, or run a different program");
                if Arc::ptr_eq(&file, &wizard) {
                    diag = diag.related(span, "allowed executables");
                }
                self.push(diag);
            }
        }

        if command.kind == CommandKind::Interactive {
            self.unattended_error(ctx, command.span, "`interactive`");
            if let Some(timeout) = &command.timeout {
                self.error(
                    &file,
                    "timeout",
                    timeout.span,
                    "`timeout` is not allowed on `interactive`",
                )
                .help = Some("a human is using the terminal; korin will not interrupt them".into());
            }
            for template in &argv {
                self.check_no_secret(
                    template,
                    "an `interactive` command, whose screen korin cannot redact",
                );
            }
        }

        let env: Vec<(String, ir::Interpolated)> = command
            .env
            .iter()
            .map(|(name, value)| (name.value.clone(), self.interpolated(ctx, value, ICtx::Env)))
            .collect();

        // Expects first, so captures can name them with `on`.
        let mut expects = Vec::new();
        let mut expect_names: HashMap<String, Sym> = HashMap::new();
        for expect in &command.expects {
            let matches = expect
                .matches
                .iter()
                .map(|p| match &p.value {
                    ast::Match::Exit(code) => ir::Match::Exit(*code),
                    ast::Match::ExitNonZero => ir::Match::ExitNonZero,
                    ast::Match::Signal(name) => ir::Match::Signal(name.clone()),
                    ast::Match::Stdout(re) => {
                        self.check_regex(&file, re, p.span, false);
                        ir::Match::Stdout(re.clone())
                    }
                    ast::Match::Stderr(re) => {
                        self.check_regex(&file, re, p.span, false);
                        ir::Match::Stderr(re.clone())
                    }
                    ast::Match::Missing(path) => {
                        ir::Match::Missing(self.interpolated(ctx, path, ICtx::Path))
                    }
                })
                .collect();
            let sym = self.new_sym(
                ctx,
                &expect.name.value,
                SymKind::Expect,
                Ty::Bool,
                expect.name.span,
            );
            self.syms[sym].producer = Some(id.to_string());
            expect_names.insert(expect.name.value.clone(), sym);
            expects.push((expect.name.clone(), ir::Expect { sym, matches }));
        }

        let secret_producing = command.captures.iter().any(|c| c.secret);
        let mut captures = Vec::new();
        let mut capture_names = Vec::new();
        for capture in &command.captures {
            match &capture.mode.value {
                CaptureMode::Jq(filter)
                | CaptureMode::JqList(filter)
                | CaptureMode::JqJson(filter) => {
                    if let Err(error) = Jq::compile(filter) {
                        self.push(Diag::error(&file, "jq", capture.mode.span, error));
                    }
                }
                CaptureMode::Regex(re) => self.check_regex(&file, re, capture.mode.span, true),
                CaptureMode::Text | CaptureMode::Lines => {}
            }
            let on = capture.on.as_ref().and_then(|on| {
                let found = expect_names.get(&on.value).copied();
                if found.is_none() {
                    self.push(
                        Diag::error(
                            &file,
                            "name",
                            on.span,
                            format!("`{}` is not an `expect` of this command", on.value),
                        )
                        .help("`on` names an `expect` declared on the same command"),
                    );
                }
                found
            });
            let file_template = capture
                .file
                .as_ref()
                .map(|path| self.interpolated(ctx, path, ICtx::Path));
            if capture.secret
                && let Some(path) = &file_template
                && !self.inside_korin_out(path)
            {
                self.error(
                    &file,
                    "secret",
                    path.loc.span,
                    "a secret file capture must read from inside `{korin.out}`",
                )
                .help = Some(
                    "korin deletes `{korin.out}` when the step ends; it cannot delete other files"
                        .into(),
                );
            }
            let ty = match capture.mode.value {
                CaptureMode::JqList(_) | CaptureMode::Lines => Ty::List,
                CaptureMode::JqJson(_) => Ty::Json,
                _ => Ty::Scalar,
            };
            let sym = self.new_sym(
                ctx,
                &capture.name.value,
                SymKind::Capture,
                ty,
                capture.name.span,
            );
            let info = &mut self.syms[sym];
            info.secret = capture.secret;
            info.optional = capture.optional;
            info.producer = Some(id.to_string());
            info.on = on;
            info.step_expects = if on.is_some() {
                Vec::new()
            } else {
                expects.iter().map(|(_, o)| o.sym).collect()
            };
            info.from_secret_step = secret_producing && !capture.secret;
            capture_names.push((capture.name.clone(), sym));
            captures.push(ir::Capture {
                sym,
                mode: capture.mode.value.clone(),
                file: file_template,
                on,
                optional: capture.optional,
                secret: capture.secret,
            });
        }
        for (name, expect) in &expects {
            self.define_name(ctx, &name.value, name.span, Binding::Sym(expect.sym));
        }
        for (name, sym) in capture_names {
            self.define_name(ctx, &name.value, name.span, Binding::Sym(sym));
        }

        // Withheld values may never reach a command; warn where one might.
        for template in argv.iter().chain(env.iter().map(|(_, t)| t)) {
            for part in &template.parts {
                if let IPart::Sym { sym, loc, .. } = part
                    && self.syms[*sym].from_secret_step
                {
                    let name = self.syms[*sym].name.clone();
                    self.push(
                        Diag::warning(&loc.file, "withheld", loc.span, format!("`{name}` may be withheld at runtime"))
                            .help("it comes from a step with a secret capture; if that secret does not resolve, korin refuses to pass this value to a command"),
                    );
                }
            }
        }

        ir::Command {
            kind: command.kind,
            exe,
            argv,
            captures,
            expects: expects.into_iter().map(|(_, o)| o).collect(),
            env,
            timeout: command.timeout.as_ref().map(|t| t.value.0),
            secret_producing,
        }
    }

    fn call(&mut self, ctx: &mut Ctx, call: &ast::Call, id: String, loc: Loc) -> Option<ir::Step> {
        let file = Arc::clone(&ctx.file);
        let Some(&module) = ctx.imports.get(&call.module.value) else {
            self.error(
                &file,
                "module",
                call.module.span,
                format!("unknown module `{}`", call.module.value),
            )
            .help = Some(format!("import it with `use {}`", call.module.value));
            return None;
        };
        let Some(define) = self.mods[module].defines.get(&call.template.value).cloned() else {
            let mut names: Vec<&String> = self.mods[module].defines.keys().collect();
            names.sort();
            let names = names
                .iter()
                .map(|n| n.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            self.error(
                &file,
                "template",
                call.template.span,
                format!(
                    "module `{}` has no template `{}`",
                    call.module.value, call.template.value
                ),
            )
            .help = Some(format!("its templates are: {names}"));
            return None;
        };
        if call.args.len() != define.params.len() {
            self.error(
                &file,
                "template",
                call.span,
                format!(
                    "`{}.{}` takes {} argument(s), but {} were given",
                    call.module.value,
                    call.template.value,
                    define.params.len(),
                    call.args.len()
                ),
            );
            return None;
        }
        if !define.exports.is_empty() && call.alias.is_none() {
            let exports: Vec<&str> = define.exports.iter().map(|e| e.value.as_str()).collect();
            self.error(
                &file,
                "template",
                call.span,
                format!(
                    "`{}.{}` exports values, so it needs `as <alias>`",
                    call.module.value, call.template.value
                ),
            )
            .help = Some(format!("its exports are: {}", exports.join(", ")));
        }
        let key = format!("{}#{}", self.mods[module].key, define.name.value);
        if let Some(position) = self.call_stack.iter().position(|k| *k == key) {
            let mut chain: Vec<String> = self.call_stack[position..]
                .iter()
                .map(|k| short_key(k))
                .collect();
            chain.push(short_key(&key));
            self.push(
                Diag::error(
                    &file,
                    "call-cycle",
                    call.span,
                    "this call makes a template call itself",
                )
                .help(format!("call chain: {}", chain.join(" → "))),
            );
            return None;
        }

        let args: Vec<(ir::Interpolated, Option<Sym>)> = call
            .args
            .iter()
            .map(|arg| {
                let template = self.interpolated(ctx, arg, ICtx::Arg);
                let list = match template.parts.as_slice() {
                    [IPart::Sym { sym, .. }] if self.syms[*sym].ty == Ty::List => Some(*sym),
                    _ => None,
                };
                (template, list)
            })
            .collect();

        self.call_count += 1;
        let alias = call
            .alias
            .as_ref()
            .map_or_else(|| format!("$call{}", self.call_count), |a| a.value.clone());
        let alias_ok = match &call.alias {
            Some(a) => self.define_name(ctx, &a.value, a.span, Binding::Alias),
            None => true,
        };

        let mut params = HashMap::new();
        for (param, (template, list)) in define.params.iter().zip(args) {
            params.insert(
                param.value.clone(),
                Binding::Arg {
                    text: template,
                    list,
                },
            );
        }
        let mod_file = Arc::clone(&self.mods[module].file);
        let mut body_ctx = Ctx {
            imports: self.mods[module].imports.clone(),
            file: Arc::clone(&mod_file),
            scopes: vec![params],
            guards: Vec::new(),
            stage: ctx.stage,
            naming: Some(Naming {
                alias: alias.clone(),
                exports: define.exports.iter().map(|e| e.value.clone()).collect(),
            }),
        };
        self.call_stack.push(key);
        let body = self.steps(&mut body_ctx, &define.body, &id);
        self.call_stack.pop();

        if let Some(a) = call.alias.as_ref().filter(|_| alias_ok) {
            for export in &define.exports {
                match body_ctx.scopes[0].get(&export.value) {
                    Some(Binding::Sym(sym)) => {
                        let sym = *sym;
                        let name = format!("{}.{}", a.value, export.value);
                        self.define_name(ctx, &name, a.span, Binding::Sym(sym));
                    }
                    _ => {
                        self.push(Diag::error(
                            &mod_file,
                            "template",
                            export.span,
                            format!(
                                "`{}` is exported but never defined in the template body",
                                export.value
                            ),
                        ));
                    }
                }
            }
        }

        let label = file.text[call.span.start..call.span.end].to_string();
        Some(ir::Step {
            id,
            loc,
            kind: ir::StepKind::Call {
                label,
                alias: call.alias.as_ref().map(|a| a.value.clone()),
                body,
            },
        })
    }
}

fn expr_syms(expr: &ir::Expr) -> Vec<Sym> {
    match expr {
        ir::Expr::Sym(s) | ir::Expr::Exists(s) => vec![*s],
        ir::Expr::Str(t) => t.syms().collect(),
        ir::Expr::List(items) => items.iter().flat_map(expr_syms).collect(),
        ir::Expr::Or(a, b) | ir::Expr::And(a, b) | ir::Expr::Cmp(_, a, b) | ir::Expr::In(a, b) => {
            let mut v = expr_syms(a);
            v.extend(expr_syms(b));
            v
        }
        ir::Expr::Not(e) | ir::Expr::Regex(e, _) => expr_syms(e),
        ir::Expr::Num(_) => Vec::new(),
    }
}

/// The file a guide diagnostic points into: the call's, else the step's.
fn diag_file(call: Option<&ir::Step>, step: &ir::Step) -> Arc<SourceFile> {
    Arc::clone(&call.map_or(&step.loc, |c| &c.loc).file)
}

/// Whether any step in `steps`, at any depth, makes a stage mutating.
pub fn steps_mutating(steps: &[ir::Step]) -> bool {
    steps.iter().any(|step| match &step.kind {
        ir::StepKind::Command(c) => c.kind.is_mutating(),
        ir::StepKind::When { body, .. }
        | ir::StepKind::Each { body, .. }
        | ir::StepKind::Poll { body, .. }
        | ir::StepKind::Call { body, .. } => steps_mutating(body),
        _ => false,
    })
}

/// Removes `.` and `..` components without touching the filesystem.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn short_key(key: &str) -> String {
    if let Some(rest) = key.strip_prefix("builtin:") {
        return rest.split('#').next().unwrap_or(rest).to_string()
            + key.find('#').map_or("", |i| &key[i..]);
    }
    let (path, template) = key
        .split_once('#')
        .map_or((key, None), |(p, t)| (p, Some(t)));
    let name = Path::new(path)
        .file_name()
        .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned());
    match template {
        Some(t) => format!("{name}#{t}"),
        None => name,
    }
}
