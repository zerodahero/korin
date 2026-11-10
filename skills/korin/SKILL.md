---
name: korin
description: Write korin wizards, `.korin` runbooks whose commands a human reviews and approves stage by stage. Use when a migration, cutover, release, cleanup or other series of commands should run under human oversight, or when writing or fixing a `.korin` file.
---

# korin

korin runs a **wizard**: a `.korin` file of stages, each a series of commands. It shows every
stage before it runs, and waits at a **gate** for a person to approve it. You write the
wizard and prove it valid; the person reads it and runs it.

[`examples/migrate.korin`](examples/migrate.korin) is a complete, annotated wizard. Read it
before you write one. [`syntax.md`](syntax.md) has every step, option and expression. The
full docs are at <https://zerodahero.github.io/korin/>.

## 1. Scope

Read the runbook, script, or task, and the repository it acts on. List every command in
order. For each, note:

- whether it **reads** or **mutates**;
- each value it produces that a later command needs, and how to extract it (ask the CLI for
  JSON: `--output json`, `--json`, `-o json`);
- each failure that is an answer rather than an error, such as "not found".

Group the commands into stages. A stage is one thing a person approves as a whole.

**Done when** every `mutate` has a `read` that tells whether its change is already made.

## 2. Write

Start from the shape of `examples/migrate.korin`. These conventions are what make a wizard
safe to approve and safe to run again:

- **Guard every change.** A `read` answers "is it done?", an `expect` names the "no" answer,
  and `when <expect>` wraps the `mutate`. A guarded wizard is idempotent: a run that stopped
  resumes by running again.
- **Make expects specific.** Pair the exit code with a regex on the message:
  `expect missing = exit 254 and stderr ~ /NotFound/`.
- **Use a plain capture only under its guard.** A capture from a step with expects is usable
  only inside `when not <expect>`, naming every expect of that step. `korin check` enforces
  this.
- **One command per step.** Each word is one argument, and `{name}` is always one argument.
  Pipes, globs, redirects, `$VAR` and `~` are literal text. Work that needs a shell goes in a
  short `script read|mutate ./name.sh` beside the wizard.
- **Pass secrets through `env`.** Declare them with `param <name> secret`, `ask secret` or
  `capture secret`, then `env NAME={name}` on the step that needs them.
- **List every program in `allow`.** Use `gates mutating` when the read-only stages are safe
  to run unasked.
- **`confirm` right before the irreversible step**, quoting the values the person should
  check.
- **`record` each change**, with its values, so the run's report says what changed.
- **Prefer `param` for inputs known at the start.** An `ask` answer blocks `--from` for
  every later stage that uses it.

## 3. Check

Run `korin check --format json <file>`. Each diagnostic has a `line`, a `message` and often a
`help` that names the fix. Fix, and run it again.

Then run `korin plan <file>`, and read it as the person will: every command with its real
arguments, each placeholder `<value ← 2.1>` pointing at the step you meant, and each stage
marked mutating or read-only as you intended. Run `korin fmt <file>` last.

**Done when** `korin check --format json` prints `[]`, the plan reads as intended, and
`korin fmt --check <file>` passes.

## 4. Hand off

Give the person:

- the wizard's path, and the exact command: `korin run <file> --param name=value ...`;
- which stages change something, and what each `confirm` asks;
- anything the wizard assumes: logged-in CLIs, the directory to run it from.

The person runs the wizard in their own terminal, where they can read and answer each gate.
A run without a terminal needs `unattended allowed` in the wizard and `--yes` on the command
line; use that only when the person asks for an unattended run, such as a CI job.
