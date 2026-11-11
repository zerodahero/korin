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

### Footer

The live line under the ghost that names the current stage and the step running now. It is
drawn only in a live terminal view and erased when the stage ends. Code: `Screen::set_footer`.

- Avoid: status bar, status line

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

The prompt a guide shows before each stage, and before an `interactive` step. Enter or Space
continues. Events: `pause`.

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
