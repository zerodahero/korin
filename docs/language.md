# The korin language

This is the reference for `.korin` files, version 1. It describes what the parser accepts,
what `korin check` rejects, and what `korin run` does.

## A first wizard

Indentation delimits blocks. Indent with spaces; a tab is an error. Comments start with `#`.

This example is `examples/intro/intro.korin`. A test keeps the two copies identical.

<!-- intro:start -->
```text
korin 1
title "RDS snapshot and restore rehearsal"
gates mutating            # optional; default `all` (see Gates)
allow aws                 # optional; only these executables may run

use aws
use rds
use "./lib/local.korin"

param cluster required
param scratch = "{cluster}-rehearsal"
param db_password secret   # prompted with hidden input if not passed

stage "Identity"
  aws.identity() as me
  confirm "Is {me.account} ({me.arn}) the right account?"

stage "Source cluster"
  rds.cluster({cluster}) as src
  assert src.status == "available" else "{cluster} is {src.status}, not available."

stage "Snapshot"
  mutate aws rds create-db-cluster-snapshot --db-cluster-identifier {cluster} --db-cluster-snapshot-identifier {scratch}-snap --output json
    capture snap_arn = jq .DBClusterSnapshot.DBClusterSnapshotArn
  rds.await_snapshot({scratch}-snap)

stage "Scratch cluster"
  read aws rds describe-db-clusters --db-cluster-identifier {scratch} --output json
    expect missing = exit 254 and stderr ~ /DBClusterNotFound/
  when missing
    mutate aws rds restore-db-cluster-from-snapshot ... --snapshot-identifier {snap_arn}
  each sg in src.security_groups
    read aws ec2 describe-security-groups --group-ids {sg} --output json
```
<!-- intro:end -->

## File structure

A wizard file has a header, then imports, then params, then stages.

```text
korin 1                       # required, first line: the language version
title "<text>"                # required
gates all | mutating          # optional, default `all`
unattended allowed            # optional, default: never
allow <name-or-path>, ...     # optional, default: any executable

use <module> [as <name>]
use "<path>" [as <name>]

param <name> required
param <name> = "<default>"
param <name> secret

stage "<title>"
  <step>
  ...
```

A module file (imported with `use "<path>"`) holds only `use` and `define`.

## Step kinds

| Step | What it does |
|---|---|
| `read <argv>` | Runs a command that does not change state. |
| `mutate <argv>` | Runs a command that changes state. |
| `script read <path> <args>` / `script mutate <path> <args>` | Runs a helper program. The keyword classifies it. |
| `interactive <argv>` | Hands the terminal to the program, for example `nvim` or `tuicr`, until it exits. |
| `confirm "<text>"` | Adds a gate in the middle of a stage. |
| `ask <name> "<prompt>"` / `ask secret <name> "<prompt>"` | Reads a value from the human. |
| `assert <expr> else "<message>"` | Stops the wizard if the expression is false. |
| `when <expr>` | Runs its indented body only if the expression is true. |
| `each <name> in <list>` | Runs its body once per item. |
| `poll every <dur> for <dur>` | Repeats its body of reads `until <expr>`, or stops on `fail <expr>`. |
| `note "<text>"` / `warn "<text>"` / `record "<text>"` | Prints a line. `record` also adds it to the report. |
| `<module>.<template>(<args>) [as <alias>]` | Expands a template in place. |

Durations are a number followed by `s`, `m` or `h`: `30s`, `15m`, `2h`.

## Step options

Options are indented under the step they belong to.

| Option | Meaning |
|---|---|
| `capture [secret] <name>[?] = <mode> [on <expect>]` | Extracts a value from stdout. |
| `capture [secret] <name>[?] = file <path> <mode> [on <expect>]` | Extracts a value from a file the command wrote. |
| `expect <name> = <match> [and <match> ...]` | Names an expected result, such as "not found". |
| `env <NAME>=<word>` | Sets an environment variable for this command only. |
| `timeout <dur>` | Limits how long the command may run. Default `15m`. Not allowed on `interactive`. |

## Commands and arguments

korin never uses a shell. It splits each command line into words on spaces, and each word
becomes one argument.

- `{name}` inside a word inserts a value. The result is still one argument, even if the
  value contains spaces, quotes or `$(...)`.
- `{list...}` inserts a list as one argument per item. It must be a whole word.
- A word in double quotes may contain spaces: `"two words"` is one argument.
- Pipes, redirects, globs and `~` have no special meaning. Use a jq capture or a `script` for
  anything that needs them.

The first word, the executable, must be static: literal text, or a template parameter that
was given literal text. It cannot come from a `param`, an `ask` or a capture. korin
resolves every executable before stage 1:

- A bare name, like `aws`, is looked up on `PATH`.
- An absolute path is used as is.
- A relative path, like `./fix.sh`, resolves against the directory of the `.korin` file that
  contains the step.

Commands run in the directory where `korin run` started, with stdin connected to
`/dev/null`.

## Values and types

Every name has a type that `korin check` knows without running anything.

| Type | Comes from | How it interpolates |
|---|---|---|
| `scalar` | params, `ask`, `text`, `regex`, `jq <filter>` | Its string form, as one argument. |
| `list` | list literals, `lines`, `jq list <filter>` | Only as `{name...}` in a command. An error in a string. |
| `json` | `jq json <filter>` | Compact JSON text, as one argument. |
| `bool` | expects | Only in `when`, `assert`, `until` and `fail`. |

Captures have no fields. Dotted names, like `src.status`, exist only for template exports.

### Capture modes

| Mode | Result |
|---|---|
| `jq <filter>` | Exactly one result, which must be a string, number or boolean. |
| `jq list <filter>` | Every result of the stream; each must be a scalar. An array result is not flattened; write `.[]`. |
| `jq json <filter>` | Exactly one result of any JSON type. |
| `text` | The whole output, with trailing whitespace removed. |
| `lines` | The non-empty lines of the output. |
| `regex /<re>/` | Group 1 of the first match. The regex must have a group. |

Filters use jq syntax, evaluated by [jaq](https://github.com/01mf02/jaq).

### When a filter finds nothing

| Case | Required capture | Optional capture (`name?`) |
|---|---|---|
| Output is not JSON | Stops. | Stops. |
| Missing key (`.Foo` gives `null`) | Stops: "filter yielded null". | Absent. |
| Explicit `null` | Stops: "filter yielded null". | Absent. |
| No results | Stops: "filter yielded no results". | Absent. |
| Several results, in scalar or json mode | Stops: "filter yielded N results". | Stops. |
| Wrong type, such as an object in scalar mode | Stops. | Stops. |

jq cannot tell a missing key from an explicit `null`, so korin treats both as absent. The jq
`?` operator keeps its jq meaning: it suppresses errors. It does not make a capture optional.

An absent capture can be used only inside `when exists <name>`. `jq list` with no results
gives an empty list, which is not absent.

## Expressions

Expressions appear in `when`, `assert`, `until` and `fail`.

- Comparison: `==`, `!=` compare string forms. `<`, `<=`, `>`, `>=` compare numbers; a value
  that is not a number stops the wizard.
- Membership: `<value> in ["a", "b"]`.
- Regex: `<value> ~ /<regex>/`.
- Logic: `and`, `or`, `not`, and parentheses.
- Presence: `exists <name>`.
- Operands: string, number and list literals, params, captures, expects and template exports.

There is no arithmetic.

## Expects

An expect names an expected result. It stops a non-zero exit from failing the step.

```text
read aws rds describe-db-clusters --db-cluster-identifier {scratch} --output json
  expect missing = exit 254 and stderr ~ /DBClusterNotFound/
```

Matches:

- `exit <n>`, `exit != 0`
- `signal <NAME>`, for example `signal INT`
- `stdout ~ /<re>/`, `stderr ~ /<re>/`
- `missing <path>`: the file is absent, or was not written by this command (see File captures)

After a command ends, korin handles the step in this order:

0. **Cancelled?** If korin ended the command itself (Ctrl-C, a timeout, a poll deadline, or
   "kill" at a suspended-program prompt), the step is cancelled and the wizard stops.
   Expects are not checked. No expect can turn a cancellation into success.
1. **Reset.** Every expect of the step is false and every capture is absent. This happens on
   every run of the step, including each poll attempt and each `each` item.
2. **Match.** Expects are tested in the order written. The first match wins.
3. **Decide.** If an expect matched, the step succeeded. Otherwise exit 0 succeeds, and
   anything else stops the wizard and shows stdout and stderr.
4. **Capture.** If no expect matched, plain captures run. If expect `o` matched, only
   captures marked `on o` run.

`korin check` enforces where captures can be used:

- A plain capture from a step with expects can be used only inside `when not o1 and not o2`,
  naming every expect of that step.
- A capture marked `on o` can be used only inside `when o`.

## Scope

- Captures in a `when` body are visible only inside that body.
- Captures in an `each` body belong to one item and are not visible after the loop.
- Captures in a `poll` body are visible after the poll, with the values from the last attempt.

## Templates and modules

A template is a named, reusable group of steps. It expands in place before korin plans
anything, so every command it runs is visible in the plan.

```text
define <name>(<param>, ...)
  exports <name>, ...
  <step>
  ...
```

```text
# rds.korin
define cluster(id)
  exports status, arn, security_groups
  read aws rds describe-db-clusters --db-cluster-identifier {id} --output json
    capture status          = jq .DBClusters[0].Status
    capture arn             = jq .DBClusters[0].DBClusterArn
    capture security_groups = jq list .DBClusters[0].VpcSecurityGroups[].VpcSecurityGroupId

# wizard
rds.cluster({cluster}) as src
rds.cluster({scratch}) as dst
assert src.status == dst.status else "{cluster} is {src.status}, {scratch} is {dst.status}."
```

- **Parameters** are positional. Each argument is one word, so `{scratch}-snap` is one argument.
- **The body sees** its parameters, its own captures and expects, and other templates. It does
  not see the caller's names.
- **`exports`** lists what the caller can use, as `<alias>.<name>`.
- **`as <alias>`** is required if the template exports anything. An alias must be unique in
  its scope.
- **Names never collide.** Each call's names are private to that call.
- **Templates can call templates**, but not themselves, directly or indirectly.
- **`use aws`** loads a module built into korin. **`use "./x.korin"`** loads a file, relative
  to the file that contains the `use`. The module's name is the file name without
  `.korin`, unless `as` renames it. Import cycles are errors.

`korin modules` lists the built-in templates.

## Gates

A gate shows a stage and asks for approval before any of its commands run.

- With `gates all` (the default), every stage has a gate.
- With `gates mutating`, only mutating stages have a gate. A stage is mutating if any step in
  it is `mutate`, `script mutate` or `interactive`, including steps inside `when`, `each`,
  `poll` and templates.
- The gate shows values known from earlier stages. A value produced inside the stage shows
  where it comes from: `<snap_arn ← 1>`.
- `confirm` adds a gate in the middle of a stage, after earlier steps have produced values.

Steps always run in the order written.

## Running without a terminal

If stdin or stdout is not a terminal, korin refuses to run anything unless the wizard says
`unattended allowed` and the run passes `--yes`. This check happens before the first command.

- `--yes` approves every gate. It is an error when a terminal is present.
- A wizard with `unattended allowed` cannot contain `confirm` or `interactive`.
- Every `required` param and every `ask` must have a value before stage 1: `--param name=value`
  for plain values, the environment variable `KORIN_SECRET_<NAME>` for secrets.

## Starting at a later stage

`korin run --from "<stage>"` skips the stages before it. korin allows this only if no later
step uses a value produced in a skipped stage: a capture, an expect, a template export or an
`ask` value. `korin plan` shows which stages are valid starting points.

## Interactive programs

`interactive <argv>` gives the terminal to a program and waits for it to exit. korin cannot
see what the program shows. It sees only the exit status and any file the program writes.

If you press Ctrl-Z inside the program, korin takes the terminal back and asks whether to
resume or kill it. Killing it cancels the step and stops the wizard.

## File captures

`capture <name> = file <path> <mode>` reads a file after the command exits.

- For each command, korin creates an empty private directory and passes it as `KORIN_OUT`
  and `{korin.out}`. A file written there is always new. korin deletes the directory when the
  step ends.
- For any other path, the file must be new or changed since the command started. An
  unchanged file counts as `missing`, so stale results from an earlier run are never used.
- A missing file stops the wizard, unless a `missing <path>` match succeeds.

## Secrets

A secret is a `param ... secret`, an `ask secret`, a `capture secret`, or any value built from
one.

- korin replaces every known secret with `«secret:name»` before anything reaches the
  terminal, the journal, the report or an output file.
- A secret cannot appear in a `note`, `record` or `assert` message, or in an `interactive`
  command.
- Commands never see `KORIN_SECRET_*` variables. A secret reaches a command only through an
  `env` line that names it.
- Pass secrets with `env NAME={secret}`, not in arguments. Other users on the same machine can
  read arguments with `ps`.
- A secret must be at least 4 characters long.

A step with a `capture secret` is handled with extra care:

- Its output is held in memory and never shown while it runs.
- Its output is published only if every secret capture on the step got a value. Otherwise the
  output is withheld and replaced by a note such as "withheld: 812 bytes; unresolved secrets:
  token".
- If its output is withheld, its other captures are **withheld values**. They can still be
  used in `when`, `assert`, `until` and `fail`, but they show as `«withheld:name»`, and korin
  refuses to pass them to any command.

korin cannot hide what an `interactive` program prints, or protect files it does not own.

## Runs, journal and report

Each run writes `.korin/runs/<time>-<wizard>/`:

- `events.jsonl`: one JSON event per stage, gate, command, capture and expect.
- `report.md`: a summary of `record` lines, approved gates and failures.
- `out/<invocation>.stdout` and `.stderr`: the full output of each command.

Every command run has an invocation ID, such as `0042-s4.1/blue.1@7`: a counter, the step's
place in the file, the template alias, the `each` index (`[i]`) and the poll attempt (`@n`).

## Resuming

korin does not replay old runs. To make a wizard safe to run again, read the current state and
guard each change with `when`:

```text
read aws rds describe-db-clusters --db-cluster-identifier {scratch} --output json
  expect missing = exit 254 and stderr ~ /DBClusterNotFound/
when missing
  mutate aws rds restore-db-cluster-from-snapshot ...
```
