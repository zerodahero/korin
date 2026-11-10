# Captures

A capture takes a value from a command's output and gives it a name. Later steps use it as
`{name}` in commands and messages, or by name in expressions.

```korin
read gh pr view 42 --json title
  capture title = jq .title
```

A capture runs only when its command succeeds: exit code 0, or an
[expect](expects.md) that matched (with `on`). It reads standard output, or a file with
`file`.

## Modes

| Mode | Gives | Type |
|---|---|---|
| `jq <filter>` | The single result of a jq filter: a string, number or boolean. | scalar |
| `jq list <filter>` | Every result of the filter, each a string, number or boolean. | list |
| `jq json <filter>` | The single result of the filter, any JSON, as compact text. | json |
| `text` | The whole output, with trailing whitespace removed. | scalar |
| `lines` | The output's lines, without empty ones. | list |
| `regex /<re>/` | Group 1 of the first match. The regex must have a group. | scalar |

Filters use jq syntax, evaluated by [jaq](https://github.com/01mf02/jaq), so no `jq` binary
is needed. Regexes use Rust's [regex syntax](https://docs.rs/regex/latest/regex/#syntax).

### jq modes

```korin title="jq.korin"
--8<-- "docs-site/examples/reference/captures/jq.korin"
```

--8<-- "docs-site/docs/generated/transcripts/reference/captures/jq.html"

- `jq` needs exactly one result. `.labels[].name` gives one result per label, so it needs
  `jq list`.
- `jq list` collects a stream. An array is one result, and is not flattened: write
  `.labels[].name`, not `.labels`.
- `jq json` keeps structure. It interpolates as compact JSON text, which suits commands that
  take JSON arguments. It has no fields: use another `jq` capture to read inside it.

### Text modes

```korin title="text.korin"
--8<-- "docs-site/examples/reference/captures/text.korin"
```

`text` suits commands that print one value. `lines` suits commands that print one item per
line, such as `kubectl get -o name`. `regex` picks a value out of text meant for people.

## Types

Each capture has a type that `korin check` knows before anything runs.

| Type | In a command | In a message | In an expression |
|---|---|---|---|
| scalar | `{name}`: one argument | `{name}` | Yes |
| list | `{name...}`: one argument per item | Not allowed | With `each`, or `in` |
| json | `{name}`: one argument | `{name}` | Compared as text |

Captures have no fields. `src.status` style names exist only for
[template exports](templates.md).

## When a filter finds nothing

| Case | Required capture | Optional capture (`name?`) |
|---|---|---|
| The output is not JSON | Stops. | Stops. |
| A missing key (`.Foo` gives `null`) | Stops: "filter yielded null". | Absent. |
| An explicit `null` | Stops: "filter yielded null". | Absent. |
| No results | Stops: "filter yielded no results". | Absent. |
| Several results, in `jq` or `jq json` | Stops: "filter yielded N results". | Stops. |
| The wrong type, such as an object in `jq` | Stops. | Stops. |

jq cannot tell a missing key from an explicit `null`, so korin treats both as absent.
`jq list` with no results gives an empty list, which is not absent.

When a capture stops the wizard, korin shows the filter and the first 20 lines of output.

## Optional captures

A `?` after the name makes a capture optional. When the filter finds nothing, the capture is
absent instead of stopping the wizard. An absent capture can be used only inside
`when exists <name>`.

```korin title="optional.korin"
--8<-- "docs-site/examples/reference/captures/optional.korin"
```

--8<-- "docs-site/docs/generated/transcripts/reference/captures/optional.html"

jq's own `?` operator keeps its jq meaning: it suppresses errors. It does not make a capture
optional.

## File captures

`capture <name> = file <path> <mode>` reads a file after the command exits, instead of
standard output. Use it for programs that write results to a file, and for
[`interactive`](steps.md#interactive) programs, whose screen korin cannot read.

```korin title="file.korin"
--8<-- "docs-site/examples/reference/captures/file.korin"
```

--8<-- "docs-site/docs/generated/transcripts/reference/captures/file.html"

- **`{korin.out}`** is an empty, private directory that korin makes for each command. The
  command also finds it in the `KORIN_OUT` environment variable. A file written there is
  always new. korin deletes the directory when the step ends.
- **Any other path** resolves against the directory korin runs in. The file must be new or
  changed since the command started. An unchanged file counts as missing, so a result left
  by an earlier run is never read by mistake.
- **A missing file** stops the wizard, unless the step has a `missing <path>` expect that
  matched. See [Expects](expects.md#matches).

korin copies each captured file into the run's [journal](journal.md), unless the capture is
secret.

## `on`: capture only when an expect matched

`capture <name> = <mode> on <expect>` runs only when that expect matched. Use it to read the
output of a result you expected, such as a "pending" status. See
[Expects](expects.md#capture-on-an-expect).

## Secret captures

`capture secret <name> = ...` makes the value a [secret](secrets.md). korin hides it
everywhere it would be shown or written, and holds the command's output back until every
secret capture of the step has a value.
