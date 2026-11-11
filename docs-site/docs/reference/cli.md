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

### Compact view

In a terminal, korin shows paths and long values in a compact view, so the commands are easy
to read:

- **Paths shorten to a prefix.** The directory korin runs in shows as `<pwd>`, `$TMPDIR` as
  `<tmp>`, and your home directory as `~`. A param, `ask` or capture whose value is an
  absolute path with at least three parts, such as `/opt/ci/build`, shows as its name:
  `<build>/app.tar.gz`. The longest prefix wins.
- **A named prefix only starts a longer path.** When the value is the whole argument, as in
  `-C {build}`, it shows in full: there, the value is the point.
- **An argument too wide for its line is cut.** The view shows its start, then how many
  characters it hides: `…(+1.2k)`.

Press `f` at a gate, a `confirm` or a guide's pause to switch to the full view, which shows
every path and value as it runs. Press `f` again to switch back. The choice holds for the
rest of the run.

```korin title="compact.korin"
--8<-- "docs-site/examples/reference/cli/compact.korin"
```

At the gate of stage 2:

--8<-- "docs-site/docs/generated/transcripts/reference/cli/compact.prompt-1.html"

The view changes only what the terminal shows. The journal always holds the full command:

```json
--8<-- "docs-site/docs/generated/transcripts/reference/cli/compact.events.jsonl:10:10"
```

A run without a terminal, such as in CI, always shows the full view.

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
- `--require-enter` makes gates and `confirm` take `y` or `n` followed by Enter, instead of a
  single key, and `f` followed by Enter switches the view. A guide's pause then continues on
  an empty line. Setting `KORIN_REQUIRE_ENTER=1` does the same.

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
run. It marks each stage as mutating or read-only, with a gate, without one, or with a
guide's pause, and whether a run can start there with `--from`. See [Gates](gates.md#korin-plan) for an example.

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
