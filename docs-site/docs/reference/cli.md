# Command line

```text
--8<-- "docs-site/docs/generated/help/korin.txt"
```

Every command takes `--color auto|always|never`. `auto` colors a terminal, unless
`NO_COLOR` is set or `TERM` is `dumb`. `CLICOLOR_FORCE` colors output that is not a terminal.

## Output

In a terminal with color, `korin run` keeps the view tidy. Once you approve a stage, its
preview folds away, and each command appears with its output under it. The steps still to
run stay dimmed below the output of the one running now. With `--color never` or `NO_COLOR`,
or when output is not a terminal, korin prints every line in order instead, and nothing is
erased. The journal is always plain text.

## `korin run`

```text
--8<-- "docs-site/docs/generated/help/run.txt"
```

- `--param name=value` gives a param, or answers an `ask` in advance. Repeat it for each
  value. Secret values are never taken here: use `KORIN_SECRET_<NAME>`.
- `--yes` approves every gate. It works only without a terminal, for a wizard that declares
  `unattended allowed`. See [Run in CI](../guides/ci.md).
- `--from <stage>` starts at a later stage, by title or number. See
  [Resume after a stop](../guides/resume.md).
- `--kill-grace <seconds>` sets how long korin waits between `SIGTERM` and `SIGKILL` when it
  stops a command.

| Exit code | Meaning |
|---|---|
| 0 | The run completed. |
| 1 | The run stopped: a gate was declined, a command failed or timed out, an `assert` was false, a poll ran out of time, or korin could not start the run. |
| 130 | The run was cancelled: Ctrl-C, or korin received `SIGTERM` or `SIGHUP`. |

When a run stops, korin prints why, the step, and the journal path.

### Ctrl-C

Ctrl-C during a command stops that command and the run: korin sends `SIGTERM` to the command
and every process it started, waits for the grace period, then sends `SIGKILL`. A second
Ctrl-C skips the wait. During an `interactive` step, Ctrl-C goes to the program, not to
korin.

## `korin check`

```text
--8<-- "docs-site/docs/generated/help/check.txt"
```

`korin check` parses the wizard, expands every template, and runs every check without running
any command. It exits 1 if it finds an error. `--from <stage>` also checks that a run can
start at that stage.

`--format json` prints diagnostics as JSON, one object per problem, for tools and coding
agents:

```json
--8<-- "docs-site/docs/generated/check-json.txt"
```

## `korin plan`

```text
--8<-- "docs-site/docs/generated/help/plan.txt"
```

`korin plan` prints every stage and step, with placeholders for values not known until the
run. It marks each stage as mutating or read-only, with a gate or without, and whether a run
can start there with `--from`. See [Gates](gates.md#korin-plan) for an example.

## `korin fmt`

```text
--8<-- "docs-site/docs/generated/help/fmt.txt"
```

See [Formatting](file.md#formatting) for what it changes.

## `korin modules`

```text
--8<-- "docs-site/docs/generated/help/modules.txt"
```

See [Standard library](stdlib.md).
