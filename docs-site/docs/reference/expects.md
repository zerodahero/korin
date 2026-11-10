# Expects

An expect names a result you expect from a command, such as "not found". When it matches,
the step succeeds whatever the exit code, and the expect's name becomes `true` for later
steps.

```korin title="not-found.korin"
--8<-- "docs-site/examples/reference/expects/not-found.korin"
```

--8<-- "docs-site/docs/generated/transcripts/reference/expects/not-found.html"

Without the expect, `gh release view` exiting 1 would stop the wizard. With it, the wizard
can check first, and create the release only when it is missing. That also makes the wizard
safe to run again: see [Resume after a stop](../guides/resume.md).

## Matches

`expect <name> = <match> [and <match> ...]`. Every match joined by `and` must hold.

| Match | Holds when |
|---|---|
| `exit <n>` | The command exited with code `n`. |
| `exit != 0` | The command exited with any code but 0. |
| `signal <NAME>` | The command died from a signal, such as `signal INT` or `signal TERM`. |
| `stdout ~ /<re>/` | Standard output matches the regex. |
| `stderr ~ /<re>/` | Standard error matches the regex. |
| `missing <path>` | The file is absent, or was not written by this command. See [File captures](captures.md#file-captures). |

```korin title="matches.korin"
--8<-- "docs-site/examples/reference/expects/matches.korin"
```

Pair an exit code with a regex when you can. Exit code 1 means many things; exit code 1 with
`release not found` means one.

## How a step ends

After a command ends, korin handles the step in this order:

1. **Cancelled?** If korin ended the command itself (Ctrl-C, a timeout, or a poll's limit),
   the step is cancelled and the wizard stops. Expects are not checked. No expect, not even
   `signal TERM`, can turn a cancellation into success.
2. **Reset.** Every expect of the step is `false`, and every capture is absent. This happens
   on every run of the step, including each poll attempt and each `each` item.
3. **Match.** Expects are tested in the order written. The first match wins; the others stay
   `false`.
4. **Decide.** If an expect matched, the step succeeded. If none did, exit code 0 succeeds,
   and anything else stops the wizard and shows the output.
5. **Capture.** If no expect matched, plain captures run. If expect `o` matched, only the
   captures marked `on o` run.

## Using captures safely

A plain capture runs only when no expect matched. `korin check` makes you prove that before
you use it: inside `when not o1 and not o2 ...`, naming every expect of that step.

```korin title="cli/invalid/unguarded.korin"
--8<-- "docs-site/examples/reference/cli/invalid/unguarded.korin"
```

--8<-- "docs-site/docs/generated/transcripts/reference/cli/invalid/unguarded.html"

The fix is the guard `korin check` suggests:

```korin
  when not missing
    note "Already published at {url}."
```

The guard must be the `when` condition itself, or one of the terms joined by `and` in it.

## Capture on an expect

`capture <name> = <mode> on <expect>` runs only when that expect matched. Use it inside
`when <expect>`. This reads the output of a result you expected, such as a list of checks
still running.

```korin title="on.korin"
--8<-- "docs-site/examples/reference/expects/on.korin"
```

--8<-- "docs-site/docs/generated/transcripts/reference/expects/on.html"

`on` takes an expect's name, not a match. Write `expect pending = exit 8`, then
`capture … on pending`. `korin check` explains the fix if you write `on exit 8`.

## In templates

An expect declared inside a template can be [exported](templates.md#exports), and the caller
uses it as `<alias>.<name>`, like `prev.missing` in the
[`aws.ssm_get`](stdlib.md) template.
