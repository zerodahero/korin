# File structure

A wizard is a `.korin` file. It has a header, then imports, then params, then stages. Blocks
are marked by indentation.

```korin title="header.korin"
--8<-- "docs-site/examples/reference/file/header.korin"
```

## Lines and indentation

- **One statement per line.** A step's options, and the body of `when`, `each` and `poll`,
  are indented under it.
- **Indent with spaces.** A tab is an error. Any consistent width works; `korin fmt` uses two
  spaces.
- **Comments** start with `#` at the start of a line or after a space, and run to the end of
  the line. A `#` inside a word, such as `page#top`, is text.

## Header

| Line | Required | Meaning |
|---|---|---|
| `korin 1` | Yes, first line | The language version. |
| `title "<text>"` | Yes | Shown at the start of the run, and used to name the run's [journal](journal.md) directory. |
| `gates all\|mutating` | No, default `all` | Which stages ask for approval. See [Gates](gates.md). |
| `mode stepwise\|guide` | No, default `stepwise` | `guide` makes a read-only walkthrough with no gates. See [Write a guide](../guides/walkthroughs.md). |
| `unattended allowed` | No | Lets the wizard run without a terminal. See [Run in CI](../guides/ci.md). |
| `allow <name-or-path>, ...` | No | The only executables the wizard may run. |

Each header line may appear only once.

### `allow`

`allow` lists every executable the wizard may start. `korin check` rejects any command whose
executable is not listed, including commands inside templates. A bare name, such as
`kubectl`, matches that name on `PATH`. A path matches the file it points to.

Use `allow` when someone else wrote the wizard, such as a coding agent. One line tells the
reader everything it can run.

## Imports

```korin
use aws                       # a module built into korin
use "./lib/k8s.korin"         # a module file, relative to this file
use "./lib/k8s.korin" as kube # the same, called `kube` instead of `k8s`
```

A module's name is its file name without `.korin`, unless `as` renames it. See
[Templates and modules](templates.md).

## Params

A param is a value given when the run starts.

```korin
param tag required                              # must be given
param namespace = "production"                  # has a default
param image = "registry.example.com/web:{tag}"  # a default can use earlier params
param registry_token secret                     # asked for with hidden input
```

- Give values with `--param name=value`. Repeat it for each param.
- A `required` param with no value is asked for at the start of the run, in a terminal.
- A `secret` param is never taken from the command line. korin reads it from
  `KORIN_SECRET_<NAME>` (here `KORIN_SECRET_REGISTRY_TOKEN`), or asks for it with hidden
  input. See [Secrets](secrets.md).

## Stages

`stage "<title>"` starts a stage. Its steps follow, indented. A stage must have at least one
step. Every stage is shown in full before it runs, and asks for approval at its
[gate](gates.md).

Stages are numbered from 1. Steps are numbered within their stage: `2.3` is the third step of
stage 2, and `2.3.1` is the first step inside it.

## Skip rules

An operator can [skip a stage](gates.md#skipping-a-stage) with `s` at its gate, or with
`--skip`. A `skip` line, before the stage's steps, says what skipping it means:

- **`skip also "<stage>", ...`** skips the named stages too. They must come later.
- **`skip never`**: the stage cannot be skipped. korin does not offer `s`, and refuses
  `--skip` for it.
- **`skip ends`**: skipping the stage ends the run, with success (exit 0).

A stage has at most one `skip` line. The preflight may have one too.

```korin title="skip.korin"
--8<-- "docs-site/examples/reference/file/skip.korin"
```

--8<-- "docs-site/docs/generated/transcripts/reference/file/skip.html"

A run that starts with `--from` takes the stages before it as done in an earlier run, so their
`skip` lines have no effect.

## Retry

A `retry` line lets a stage run again when one of its steps fails. It suits steps that fail
now and then for reasons outside the wizard, such as a smoke test, or a `poll` that did not
wait long enough. Like `skip`, it goes before the stage's steps; the two can come in either
order.

```korin
stage "Smoke test"
  retry
  read curl -fsS https://shop.example.com/health
```

When a step fails, the stage asks:

```text
r retry from 4.2 · R retry stage · q quit
```

- **`r`** runs the stage again from the step that failed. A step inside `when`, `each` or
  `poll` runs again from the top-level step that holds it. Values that earlier steps
  captured keep their values.
- **`R`** runs the whole stage again, from its first step.
- **`q`**, Enter or Esc stops the run, as a failure does in any other stage.

A retry needs no new gate, and has no limit. The run log marks each one with
`↻ retry 2 of stage 4, from 4.2`, and the run's summary says how many times the stage was
retried. A stage is retried only when it failed: never after Ctrl-C, and never without a
terminal or under `--yes`, where there is no one to ask.

A stage has at most one `retry` line. The preflight may have one too.

## Preflight

`preflight` starts a block of lookups that every run needs: account IDs, the current state,
what is already done. It comes after the header and params, before the first stage, and a
wizard has at most one. A title is optional: `preflight "Which account?"`. Without one, the
title is "Preflight".

```korin title="preflight.korin"
--8<-- "docs-site/examples/reference/file/preflight.korin"
```

--8<-- "docs-site/docs/generated/transcripts/reference/file/preflight.html"

- **It only reads.** `korin check` rejects `mutate` and `script mutate` in it, even inside a
  template. `interactive`, `ask` and `confirm` are allowed, but keep a preflight to `read`
  and `script read`, so it can run at the start of every run without asking anything.
- **It runs first, even with `--from`.** Its values are always there, so a later stage that
  reads them never blocks a [start with `--from`](../guides/resume.md#start-at-a-later-stage).
- **It has no number.** It shows as "Preflight", and its steps are `p.1`, `p.2` and so on.
  Stage numbers, and the names `--from` takes, stay the same whether or not a wizard has a
  preflight.
- **It is shown before it runs**, like a stage that only reads: a gate under `gates all`, and
  a pause under `gates mutating` or `mode guide`.

The same wizard, started with `--from Restart`:

--8<-- "docs-site/docs/generated/transcripts/reference/file/preflight-from.html"

## Module files

A module file holds only `use` and `define` lines, and no header. See
[Templates and modules](templates.md).

```korin title="lib/k8s.korin"
--8<-- "docs-site/examples/reference/templates/lib/k8s.korin"
```

## Commands and arguments

korin never uses a shell. It splits each command line into words on spaces, and each word
becomes one argument.

```korin title="arguments.korin"
--8<-- "docs-site/examples/reference/file/arguments.korin"
```

`korin plan` shows the arguments each command will get, quoted where needed:

--8<-- "docs-site/docs/generated/transcripts/reference/file/arguments.html"

The rules:

- **`{name}` inside a word inserts a value.** The result is still one argument, even if the
  value holds spaces, quotes or `$(...)`. Nothing in a value is ever run.
- **`{list...}` inserts a list** as one argument per item. It must be a whole word.
- **Double quotes** make one argument from text with spaces: `"fix: retry"`.
- **A backslash** keeps the next character literal: `\{`, `\}`, `\"`, `\\`, `\#`, `\,`
  and `\)`, and a backslash before a space keeps the space in the word. `\n` and `\t` are a
  newline and a tab.
- **Pipes, redirects, globs, `~` and `$VAR`** have no special meaning. `*.log` is passed as
  the text `*.log`. Use a [jq capture](captures.md) or a [`script`](steps.md#script) for
  work that needs a shell.

### The executable

The first word is the executable. It must be known before the run starts, so it cannot come
from a param, an `ask` or a capture. It can be literal text, or a template parameter given
literal text.

korin finds every executable before stage 1:

- A bare name, such as `kubectl`, is looked up on `PATH`.
- An absolute path is used as is.
- A relative path, such as `./scripts/fix.sh`, resolves against the directory of the `.korin`
  file that holds the step.

If one is missing or not executable, the run stops before any command runs. A later change to
`PATH` cannot swap a program mid-run.

### Where commands run

Commands run in the directory where you started `korin run`. Their standard input is
`/dev/null`, so a command that waits for input gets end-of-file at once. Use
[`interactive`](steps.md#interactive) for programs that need the terminal.

## Formatting

`korin fmt <files>` rewrites files in the standard layout. It indents two spaces per level,
trims trailing spaces, keeps at most one blank line in a row, puts a blank line before each
`stage` and `define`, and aligns the `=` of consecutive `capture` lines. Comments are kept. `korin fmt --check <files>` lists files that are not formatted,
and changes nothing.
