# Glossary

Domain terms for korin. Name new code and prose with these words. Code words are the
identifiers; the UI label is the word a wizard author or operator sees in the language,
the CLI output and the docs.

## Terms

### Wizard

A `.korin` file with a header, params and stages; the thing a human runs.

- UI label: wizard
- Avoid: runbook (as a noun for the file; fine as the product tagline)

### Program

A wizard after loading, template expansion and checking: names resolved, ready to plan or
run. Code only.

- Avoid: Compiled, CheckedWizard

### Stage

A titled group of steps that is shown, and gated, as one unit.

- UI label: stage
- Avoid: Phase

### Step

One line of a stage or template body: a command, a prompt, a guard, a loop or a message.

- UI label: step
- Avoid: Action

### Gate

The approval prompt that shows a stage and waits for y/N before any of its commands run.
`confirm` adds one mid-stage.

- UI label: gate
- Avoid: Approval

### Capture

A value extracted from a command's output, or from a file it wrote, with jq, text, lines or
regex.

- UI label: capture
- Avoid: Extract

### Expect

A named, expected result of a command, such as "not found". When one matches, a non-zero
exit does not fail the step. Expects are mutually exclusive; the first match wins.

- UI label: expect
- Avoid: Outcome

### Match

One test inside an `expect`: an exit code, a signal, a regex on stdout or stderr, or a
missing file.

- UI label: match
- Avoid: Predicate, Condition

### Regex test

The `<value> ~ /<regex>/` expression in a condition. Code: `Expr::Regex`.

- UI label: regex
- Avoid: Match (that word names an `expect` test)

### Condition

The true-or-false expression after `when`, `assert`, `until` and `fail`.

- UI label: condition

### Interpolated

An argv word or a quoted string, made of literal text and `{name}` interpolations. Its
parts are `IPart`. Code only.

- Avoid: Template (that word names a `define`)

### Template

A named, reusable group of steps, declared with `define` in a module and expanded in place
at each call. Code: `Define`.

- UI label: template
- Avoid: Template (as a code type; see Interpolated)

### Module

A file of templates, loaded with `use`: built in (`aws`, `rds`) or a path. A module the
compiler has loaded is a `LoadedModule`.

- UI label: module
- Avoid: Mod

### Alias

The name a template call is given with `as`, under which its exports are visible.

- UI label: alias
- Avoid: handle

### Export

A capture or expect that a template makes visible to its caller as `<alias>.<name>`.

- UI label: export
- Avoid: return

### Param

A wizard input, given before stage 1 with `--param`, `KORIN_SECRET_<NAME>`, a default or a
prompt. A template's inputs are its parameters.

- UI label: param
- Avoid: Input

### Reference

One place where a step reads a value produced at run time, recorded for the `--from`
check. Code only.

- Avoid: Use (that word names a `use` import line)

### Guard

A `when` condition, and what the checker learns from it (`not missing`, `missing`,
`exists x`), that makes a capture safe to read.

- UI label: guard
- Avoid: Fact

### Withheld value

A capture from a step whose output was withheld because a secret did not resolve. Usable
inside korin, never shown, never passed to a command.

- UI label: withheld
- Avoid: tainted, taint

### Secret-producing step

A command with any `capture secret`. Its output is held in memory and published
all-or-nothing. Code: `secret_producing`.

- Avoid: captures_secret

### Redactor

The single component every sink writes through; it replaces each known secret with
`«secret:name»`. Code only.

- Avoid: Masker

### Journal

The run directory `.korin/runs/<time>-<wizard>/`, holding `events.jsonl`, `report.md` and
`out/`.

- UI label: journal
- Avoid: run directory

### Invocation

One execution of a command step. Repeats in `each` and `poll` are separate invocations,
each with an ID such as `0042-s4.1/blue.1@7`.

- UI label: invocation
- Avoid: execution

### Scratch directory

The private, per-invocation directory passed as `KORIN_OUT` and `{korin.out}`, deleted when
the step ends. Code: `scratch`.

- UI label: scratch directory
- Avoid: output directory, out_dir

### Handoff

Giving the terminal to an `interactive` program and taking it back, with the job-control
protocol. Events: `handoff_start`, `handoff_end`.

- UI label: interactive

### Allow list

The optional `allow` header listing every executable a wizard may run. Code: `Allow`.

- UI label: `allow` list
- Avoid: allowlist

### Sym

A resolved name: a param, ask, capture, expect or loop variable, unique after expansion.

- UI label: name
- Avoid: Symbol

### Exe

The program a command runs, resolved once before stage 1.

- UI label: executable
- Avoid: Executable (as a code type)

### Say step

A `note`, `warn` or `record` step: a line shown to the human, and for `record` added to
the report. Code: `Say`.

- Avoid: Message

### Plan pane

The lines under the run log while a stage runs: the stages that ended, the current stage with
the step running now marked `▶`, and the stages to come. It takes about a third of the
terminal, is drawn only in a live terminal view, and is erased when the stage ends. Code:
`Screen::set_pane`, `pane::Plan`.

- UI label: plan
- Avoid: ghost, footer, status bar

### Run log

What a run leaves in the terminal's scrollback, written like a shell session: a `$` line for
each command, its output, `↳` notes under it, and `#` lines for korin's own events.

- UI label: run log
- Avoid: transcript (the docs site's saved runs), output (a command's own stdout and stderr)

### Live window

The lines above the plan pane while a command runs: its `$` line and the newest lines of its
output. It goes once the command ends, and the command's block goes into the run log. A
secret-producing command shows only a "held" line there. Code: `Screen::set_window`.

- UI label: live window
- Avoid: tail, preview

### Fold

How a long, successful command's output shows in the run log: its first lines, then
`↳ ok · +N lines · <journal file>`. A failed command, or a poll try that failed, shows in
full. The full output stays in the command's spool for `o`, and in the journal. Code:
`log::Mode::Folded`.

- UI label: fold
- Avoid: collapse, truncate

### Spool

One command's complete, redacted output, in the order its lines arrived, in a file under the
run's own scratch root. Full rendering and `o` read it. It lives until the run ends. Code:
`log::Spool`.

- Avoid: buffer, cache

### Mode

The `mode` header line: `stepwise` or `guide`. Code: `Mode`.

- UI label: mode

### Stepwise mode

The default mode: each stage is previewed and, as `gates` says, approved before it runs.

- UI label: stepwise

### Guide mode

A read-only wizard: no previews and no gates, and a pause before each stage. `korin check`
rejects `mutate` and `script mutate` in it, even inside templates.

- UI label: guide
- Avoid: tour, walkthrough (as code names; fine in prose)

### Pause

The prompt a guide shows before each stage, and before an `interactive` step, and that a
read-only stage shows under `gates mutating`. Enter or Space continues. Events: `pause`.

- UI label: pause
- Avoid: Gate (a gate asks y/N; a pause only waits)

### Compact view

How the terminal shows a run by default: paths shortened to a path prefix, and arguments
too wide for their line cut with `…(+N)`. The journal never uses it. Code: `View::Compact`.

- UI label: compact
- Avoid: short view, abbreviated

### Full view

The terminal shows every path and value as it runs. `f` at a gate, a `confirm` or a pause
switches between the views. Runs without a terminal use it. Code: `View::Full`.

- UI label: full

### Path prefix

A directory the compact view shows by name: `<pwd>`, `<tmp>`, `~`, or `<name>` for a param,
ask or capture holding an absolute path with at least three components. Code: `Prefixes`.

- UI label: `<name>/`
- Avoid: alias (that word names a template call's `as`)

### Legend

The compact view's `<name> = <path>` lines, which say what each path prefix stands for. `l`
at a prompt hides or shows them. Never in the journal. Code: `Runner::say_legend`.

- UI label: legend
- Avoid: key

### Config

The user's `config.kdl` of reader preferences, such as the starting view. Never a project
file, and never anything that changes what a wizard does. Code: `Config`.

- UI label: config file
- Avoid: settings, preferences (as code names)

### Values view

The list `v` opens at a gate, a `confirm` or a pause: every value known so far, each of which
the operator can change. Code: `Runner::values_view`.

- UI label: values
- Avoid: params view (it lists more than params)

### Set by hand

A value the operator gave in the values view or the skip dialog, or that `--set` gave.
Event: `value_set`.

- UI label: set by hand

### Preflight

The read-only block before stage 1, for lookups every run needs. It runs first, even with
`--from`, and has no number: its steps are `p.1`, `p.2`. Code: `Program.preflight`, a
`Stage` numbered 0.

- UI label: preflight

### Skip

An operator's choice not to run a stage, and to go on with the run: `s` at a prompt, or
`--skip`. Not a declined gate, which stops the run. Events: `stage_skip`.

- UI label: skip
- Avoid: bypass

### Skip rule

A stage's `skip never`, `skip ends` or `skip also` line. Code: `SkipRule`.

- UI label: `skip` line

### Dependent

A stage that cannot run because a skip leaves a value it reads unset, or because a skipped
stage names it in `skip also`. Code: `skip::Closure`.

- UI label: dependent
- Avoid: downstream

### Pin

A script's bytes and hash, read once when its stage first shows a prompt. Previews show the
pinned bytes, and a script whose file no longer matches its pin does not run. Code: `Pin`.

- UI label: none; the journal's `script_sha256`
- Avoid: lock

### Retry

Running a failed stage again, from the top-level step that failed (`r`) or from its first
step (`R`). Only a stage with a `retry` line offers it. Code: `ir::Stage::retry`,
`Runner::stage_steps`.

- UI label: retry
- Avoid: rerun, resume (resume is `--from`)

### Exit tag

The `· exit 0` that ends a command's block in the run log, in the color of how the command
ended. Code: `log::Exit`.

- UI label: none
- Avoid: status line

### Opener

The program `o` opens an item with, chosen by the item's kind: script, json, markdown or
text. Set in the config's `open` block; the pager when unset. Code: `log::OpenKind`,
`Runner::opener_argv`.

- UI label: `open` (config)
- Avoid: viewer, handler
