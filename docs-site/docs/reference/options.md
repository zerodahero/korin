# Step options

Options are indented under the command step they belong to: `read`, `mutate`, `script` or
`interactive`.

| Option | Meaning |
|---|---|
| `capture [secret] <name>[?] = <mode> [on <expect>]` | Takes a value from the command's output. See [Captures](captures.md). |
| `capture [secret] <name>[?] = file <path> <mode> [on <expect>]` | Takes a value from a file the command wrote. See [File captures](captures.md#file-captures). |
| `expect <name> = <match> [and <match> ...]` | Names a result you expect, such as "not found". See [Expects](expects.md). |
| `env <NAME>=<word>` | Sets an environment variable for this command only. |
| `timeout <duration>` | Limits how long the command may run. |

## `env`

`env NAME=<word>` sets one environment variable for this command. The value is one word, and
can use `{references}`. Write one `env` line per variable.

```korin title="env.korin"
--8<-- "docs-site/examples/reference/options/env.korin"
```

--8<-- "docs-site/docs/generated/transcripts/reference/options/env.html"

`env` is the way to give a command a [secret](secrets.md). Any user on the same machine can
read a command's arguments with `ps`, but not its environment. The gate shows the variable
with the secret hidden.

Commands inherit korin's own environment, except the `KORIN_SECRET_*` variables. A secret
reaches a command only through an `env` line that names it.

## `timeout`

`timeout <duration>` cancels the command if it runs longer. The default is `15m` for `read`,
`mutate` and `script`.

```korin title="timeout.korin"
--8<-- "docs-site/examples/reference/options/timeout.korin"
```

When a command times out, korin stops it and the wizard stops:

1. korin sends `SIGTERM` to the command and every process it started.
2. It waits for the grace period: 10 seconds, or `--kill-grace <seconds>`.
3. It sends `SIGKILL` to whatever is left.

A timeout always stops the wizard, with exit code 1. No expect can turn it into success. A cancelled `mutate`
may have changed part of what it meant to change, and korin says so.

`interactive` steps take no `timeout`, because a person is using the program.
