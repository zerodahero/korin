# korin syntax, version 1

Indentation marks blocks; indent with spaces. `#` after a space starts a comment.

## File

```text
korin 1                        # first line
title "<text>"
gates all | mutating           # optional; default all
unattended allowed             # optional; no confirm or interactive allowed then
allow <program>, ...           # optional; the only programs that may run

use aws | rds                  # built-in modules
use "./lib/<name>.korin" [as <alias>]

param <name> required
param <name> = "<default, may use {other_param}>"
param <name> secret            # from KORIN_SECRET_<NAME>, or asked with hidden input

stage "<title>"
  <steps>
```

A module file holds only `use` and `define`.

## Steps

| Step | Notes |
|---|---|
| `read <program> <args>` | Changes nothing. |
| `mutate <program> <args>` | Changes something; makes the stage mutating. |
| `script read\|mutate ./x.sh <args>` | Path relative to the `.korin` file. |
| `interactive <program> <args>` | Hands over the terminal (editors, TUIs). No `timeout`. |
| `confirm "<question>"` | Asks y/N mid-stage with one key; n, Enter or Esc stops. |
| `ask [secret] <name> "<prompt>"` | Reads a value; `--param name=...` answers it ahead. |
| `assert <expr> else "<message>"` | Stops the run when false. |
| `when <expr>` | Body runs when true. No `else`: write the opposite `when`. |
| `each <name> in <list>` | Body runs per item. |
| `poll every <dur> for <dur>` | Body of `read`s, then `until <expr>`, optional `fail <expr>`. |
| `note` / `warn` / `record "<text>"` | `record` also goes to the report. |
| `<module>.<template>(<arg>, ...) [as <alias>]` | Exports become `<alias>.<name>`. |

Durations: `30s`, `15m`, `2h`; more than zero, at most `720h`.

## Options, indented under a command step

```text
capture [secret] <name>[?] = <mode> [on <expect>]
capture [secret] <name>[?] = file <path> <mode> [on <expect>]
expect <name> = <match> [and <match> ...]
env <NAME>=<word>
timeout <dur>                  # default 15m
```

Capture modes:

| Mode | Gives |
|---|---|
| `jq <filter>` | Exactly one string, number or boolean. |
| `jq list <filter>` | Every result of the stream, as a list. Write `.items[]`, not `.items`. |
| `jq json <filter>` | Exactly one result of any JSON type, as compact text. |
| `text` | All output, trailing whitespace trimmed. |
| `lines` | Non-empty lines, as a list. |
| `regex /<re>/` | Group 1 of the first match. `$` matches only at the very end of the output; write `(?m)` for line anchors. |

- `name?` makes a capture optional: absent instead of failing when jq finds nothing or
  `null`. Use it only inside `when exists name`.
- Plain captures run only when no expect matched; `on <expect>` captures run only when that
  one matched, and are used inside `when <expect>`.
- `file {korin.out}/<file>` reads a file the command wrote into its private scratch directory
  (also `$KORIN_OUT`).

Matches: `exit <n>`, `exit != 0`, `signal <NAME>`, `stdout ~ /re/`, `stderr ~ /re/`,
`missing <path>`. The first expect that matches wins.

## Values

- `{name}` inside a word: one argument. Text around it stays in the argument.
- `{list...}` as a whole word: one argument per item.
- `"two words"`: one argument. Escapes: `\{ \} \" \\ \# \, \)`, `\n`, `\t`, and a backslash
  before a space.
- The program, the first word, must be literal text.
- Lists cannot appear in strings. Captures have no fields; dotted names are template exports.

## Expressions

`==` `!=` (text), `<` `<=` `>` `>=` (numbers), `x in ["a", "b"]` or `x in list`,
`x ~ /re/`, `exists x`, `and`, `or`, `not`, parentheses. An expect's name is a boolean. No
arithmetic.

## Templates

```text
define <name>(<param>, ...)
  exports <capture-or-expect>, ...   # optional, first line of the body
  <steps>
```

Parameters work like params inside the body. Pass a list as `({items})`. The body cannot see
the caller's names.

## Commands

```text
korin check --format json <file>   # [] when valid
korin check <file> --from <stage>  # can a run start there?
korin plan <file>                  # every step, with placeholders for unknown values
korin fmt [--check] <files>
korin modules                      # built-in templates
korin run <file> --param k=v ...   # for the person, in a terminal
```
