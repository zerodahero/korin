# Command line

```text
--8<-- "docs-site/docs/generated/help/korin.txt"
```

Every command takes `--color auto|always|never`. `auto` colors a terminal, unless
`NO_COLOR` is set or `TERM` is `dumb`. `CLICOLOR_FORCE` colors output that is not a terminal.

Your [config file](config.md) can set the color, the starting view and `require-enter` for
every run.

## Output

`korin run` writes a run log that reads like a shell session, in your terminal's own
scrollback:

- **Each command is a `$` line** that you can paste into a shell, with its output under it.
  A secret shows as a shell variable, such as `"${TOKEN}"`; see
  [Secrets in the run log](secrets.md#secrets-in-the-run-log). The step's scratch directory
  shows as `${KORIN_OUT}`.
- **Each command ends with an exit tag**, such as `· exit 0`, on its last output line, or on
  the `$` line when it printed nothing. The `$` and the tag share a color: green for exit 0,
  the warning color for a non-zero exit that an `expect` accepted (`exit 1 (expected)`), a
  signal or a cancel, and red for a failure or a timeout. Without a terminal, the tag takes a
  line of its own.
- **`↳` notes** under a command say which expect matched, and where a long output went.
- **Each stage opens with a banner** that says how its gate went, such as
  `━━ 2/4 Snapshot ━━━━ approved ━━`. Everything in the stage sits indented under it, and
  it closes with `✓ stage 2 done · 2 commands · 3m40s`.
- **Captures show on one `→` line** under the command, such as `→ ready = 3  ·  wanted = 3`.
  Values you gave show the same way: `→ region = us-east-1  (asked)`.
- **`note`, `warn` and `record` show as messages**, each line after a `▌` bar, so they never
  read as command output: a note in italic, a warning after `▌⚠`, and a record after `▌✓`.
- **A long output folds.** A successful command with more than 12 lines shows its first 4,
  then `↳ exit 0 · +N lines · <journal file>`. A failed command shows every line. Press `o`
  at any prompt to open a folded output in full; see [Opening an item](#opening-an-item).
- **A poll leaves one line per try**, such as `↻ try 2 · 1s  ready = 2`, then the try that
  matched in full. See [`poll`](steps.md#poll).
- **The run ends with a summary**: each stage, how it ended and how long it took.

In a terminal with color, the plan pane sits under the run log while a stage runs. It shows
the stages that ended, the current stage with the step running now marked `▶`, and the
stages to come. Its steps show without their kind, as `3.2 kubectl get nodes`; a step that
changes things shows `⚠` before its command. A running command shows its newest lines in a
live window above the pane. Gates, `confirm`, pauses and dialogs show in the pane too, and
are erased once you answer, so the run log keeps one line for each.

Every prompt in the pane sits in a box, so a glance finds it under the run log:

```text
╭──────────────────────────────────────────────────────────────╮
│ ? Run stage 2? [y/N · s skip · v values · f full · l legend] │
╰──────────────────────────────────────────────────────────────╯
```

Set `icons nerd` in your [config file](config.md) to mark the pane with Nerd Font glyphs.

With `--color never` or `NO_COLOR`, or when output is not a terminal, there is no pane and
nothing is erased: korin prints every line in order, prints what a prompt asks about before
it asks, and folds nothing. The journal is always plain text.

### Compact view

In a terminal, the plan pane and the gates show paths and long values in a compact view, so
the commands are easy to read. The run log always shows them in full, so its `$` lines can be
pasted.

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

#### The legend

A legend says what each prefix stands for, as `<build> = ~/project/build`. It shows:

- under each gate's preview, for every prefix the preview uses;
- in the plan pane while a stage runs, for every `<name>` known so far.

`~` never gets a legend line. Press `l` at a gate, a `confirm` or a pause to hide the legend,
and again to show it. The choice holds for the rest of the run. Set `legend #false` in your
[config file](config.md) to start with it hidden. The full view has no legend.

```korin title="compact.korin"
--8<-- "docs-site/examples/reference/cli/compact.korin"
```

At the gate of stage 2:

--8<-- "docs-site/docs/generated/transcripts/reference/cli/compact.prompt-2.html"

The view changes only what the plan pane and the gates show. The run log and the journal
always hold the full command:

```json
--8<-- "docs-site/docs/generated/transcripts/reference/cli/compact.events.jsonl:11:11"
```

A run without a terminal, such as in CI, always shows the full view.

### Opening an item

Press `o` at a prompt to open a script of the stage to come, or an output folded so far.
With more than one, korin lists them, each with its kind:

- **`script`**: a script step's file.
- **`markdown`**: a `.md` script, or the output of a command with an `output markdown` line
  (see [`output`](options.md#output)).
- **`json`**: an output whose stdout parses as JSON, or one whose command has an
  `output json` line. Its opener gets stdout alone, so stderr cannot break the format.
- **`text`**: any other output.

Each kind opens in the program your [config file](config.md#keys)'s `open` block names for
it, such as `nvim -R` for scripts or `jnv` for JSON. A kind with no program opens in the
pager. The program gets a redacted copy, named with the kind's extension.

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
- `--skip <stage>` skips a stage, by title or number. Repeat it for each stage. See
  [Skipping a stage](gates.md#skipping-a-stage).
- `--set name=value` gives a capture or an expect of a skipped stage, so the stages that
  read it can still run. A list is a JSON array of strings, JSON is JSON text, and an expect
  is `true` or `false`. A secret capture takes `KORIN_SECRET_<NAME>` instead.
- `--skip-preflight` skips the [preflight](file.md#preflight).
- `--script-lines <n>` shows a script in full at its gate when it has at most `n` lines. The
  default is 20, and `0` never shows one. See [`script`](steps.md#what-the-gate-shows).
- `--kill-grace <seconds>` sets how long korin waits between `SIGTERM` and `SIGKILL` when it
  stops a command.
- `--require-enter` makes gates and `confirm` take `y` or `n` followed by Enter, instead of a
  single key, and `f` followed by Enter switches the view. A guide's pause then continues on
  an empty line. Setting `KORIN_REQUIRE_ENTER=1` does the same.

korin checks `--from`, `--skip`, `--skip-preflight` and `--set` before anything runs, even
the preflight. Without a terminal, a skip that leaves a later stage without a value it reads
stops the run there: give the value with `--set`, or skip that stage too.

| Exit code | Meaning |
|---|---|
| 0 | The run completed, or ended early because a `skip ends` stage was skipped. |
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
