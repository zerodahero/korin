# Steps

A stage holds steps, and runs them in the order written. Each step is one line. Options and
bodies are indented under it.

| Step | What it does |
|---|---|
| [`read <argv>`](#read-and-mutate) | Runs a command that does not change anything. |
| [`mutate <argv>`](#read-and-mutate) | Runs a command that changes something. |
| [`script read\|mutate <path> <args>`](#script) | Runs a helper program that ships beside the wizard. |
| [`interactive <argv>`](#interactive) | Hands the terminal to a program, such as an editor, until it exits. |
| [`confirm "<text>"`](#confirm) | Asks a yes/no question in the middle of a stage. |
| [`ask [secret] <name> "<prompt>"`](#ask) | Reads a value from the person running the wizard. |
| [`assert <expr> else "<message>"`](#assert) | Stops the wizard if the expression is false. |
| [`when <expr>`](#when) | Runs its body only if the expression is true. |
| [`each <name> in <list>`](#each) | Runs its body once per item. |
| [`poll every <dur> for <dur>`](#poll) | Repeats its body until a condition holds. |
| [`note`, `warn`, `record`](#note-warn-and-record) | Print a line. `record` also adds it to the report. |
| [`<module>.<template>(<args>)`](templates.md) | Expands a template in place. |

## `read` and `mutate`

`read` and `mutate` run one command. The rest of the line is the command: korin splits it
into words, and each word is one argument. There is no shell. See
[Commands and arguments](file.md#commands-and-arguments).

The keyword tells korin, and the person reading the gate, whether the command changes
anything. korin does not check this. A `mutate` step makes its stage
[mutating](gates.md#gates-mutating).

```korin title="deploy.korin"
--8<-- "docs-site/examples/reference/steps/deploy.korin"
```

At the gate, before an answer:

--8<-- "docs-site/docs/generated/transcripts/reference/steps/deploy.prompt-1.html"

The run:

--8<-- "docs-site/docs/generated/transcripts/reference/steps/deploy.html"

At the gate, every step of the stage is shown with its kind. Once you approve, the preview
is erased, and each command appears with its output under it.

## `script`

`script read` and `script mutate` run a helper program. The keyword after `script` is
required, and classifies the step as `read` or `mutate` does.

A relative path, such as `./scripts/rotate-logs.sh`, resolves against the directory of the
`.korin` file, not the directory you run korin in. Helpers can therefore ship beside the
wizard.

```korin title="script.korin"
--8<-- "docs-site/examples/reference/steps/script.korin"
```

```sh title="scripts/rotate-logs.sh"
--8<-- "docs-site/examples/reference/steps/scripts/rotate-logs.sh"
```

Use `script` for work that needs a shell: pipes, globs, redirects or loops. The gate shows
the script's path and arguments, not its contents, so keep scripts short and next to the
wizard where a reviewer can read them.

## `interactive`

`interactive` gives the terminal to a program, and waits for it to exit. Use it for
editors, pagers and review tools: `nvim`, `less`, `glow`, `tig`.

```korin
--8<-- "docs-site/examples/reference/steps/interactive.korin"
```

- korin does not see what the program shows. It sees only the exit code, and files the
  program writes. Use a [file capture](captures.md#file-captures) to read results back.
- An `interactive` step makes its stage mutating, because an editor can change files.
- `timeout` is not allowed on `interactive`.
- A wizard with `interactive` cannot run [unattended](../guides/ci.md).

See [Hand off to an editor or TUI](../guides/handoff.md) for Ctrl-Z and file captures.

## `confirm`

`confirm` asks a yes/no question in the middle of a stage. Use it after a `read` has found
a value that the person should check before the next step uses it.

```korin title="confirm.korin"
--8<-- "docs-site/examples/reference/steps/confirm.korin"
```

At the `confirm`, before an answer:

--8<-- "docs-site/docs/generated/transcripts/reference/steps/confirm.prompt-2.html"

The run:

--8<-- "docs-site/docs/generated/transcripts/reference/steps/confirm.html"

The question shows values found so far, and lists the steps that will run next. Any answer
but `y` or `yes` stops the wizard, and nothing after the `confirm` runs.

## `ask`

`ask <name> "<prompt>"` reads a value from the person running the wizard. The value is a
scalar, used like a param. `ask secret` reads it with hidden input, and treats it as a
[secret](secrets.md).

```korin title="ask.korin"
--8<-- "docs-site/examples/reference/steps/ask.korin"
```

The run:

--8<-- "docs-site/docs/generated/transcripts/reference/steps/ask.html"

`--param ticket=OPS-481` answers the `ask` in advance. Without a terminal, every `ask` must
be answered this way, or with `KORIN_SECRET_<NAME>` for `ask secret`.

## `assert`

`assert <expr> else "<message>"` stops the wizard with the message when the expression is
false. Use it to check what a `read` found before anything changes.

```korin title="assert.korin"
--8<-- "docs-site/examples/reference/steps/assert.korin"
```

The run:

--8<-- "docs-site/docs/generated/transcripts/reference/steps/assert.html"

The run stops with exit code 1. See [Expressions](expressions.md) for what an expression can
test.

## `when`

`when <expr>` runs its indented body only if the expression is true. There is no `else`:
write a second `when` with the opposite condition.

```korin title="when.korin"
--8<-- "docs-site/examples/reference/steps/when.korin"
```

The run:

--8<-- "docs-site/docs/generated/transcripts/reference/steps/when.html"

A `when` body is numbered under its `when`: the `kubectl delete` above is step `1.2.1`.
Captures made inside a body are visible only inside it. `when` is also how a wizard reacts
to an [expect](expects.md), and how it stays safe to run again
(see [Resume after a stop](../guides/resume.md)).

## `each`

`each <name> in <list>` runs its body once per item. The list is a list literal, a `lines`
capture, a `jq list` capture, or a list-typed template export.

```korin title="each.korin"
--8<-- "docs-site/examples/reference/steps/each.korin"
```

The run:

--8<-- "docs-site/docs/generated/transcripts/reference/steps/each.html"

Captures inside an `each` body belong to one item, and are not visible after the loop.

## `poll`

`poll every <interval> for <limit>` runs its body, then waits and runs it again, until the
`until` expression is true. The body holds `read` steps only. It must end with `until`, and
may add `fail`:

- `until <expr>`: the poll succeeds when this is true.
- `fail <expr>`: the wizard stops at once when this is true. Use it for states that will
  never recover.

If the limit passes first, the wizard stops, and shows the last value of every capture in
the body.

```korin title="poll.korin"
--8<-- "docs-site/examples/reference/steps/poll.korin"
```

The run:

--8<-- "docs-site/docs/generated/transcripts/reference/steps/poll.html"

Captures from the last attempt are visible after the poll. Each attempt starts fresh: an
expect or capture from an earlier attempt never carries over. A command that is still
running when the limit passes is cancelled.

## `note`, `warn` and `record`

All three print a line. `warn` marks it as a warning. `record` also writes it to the run's
`report.md`, so use it for facts you want to keep: what was changed, and to what.

```korin title="messages.korin"
--8<-- "docs-site/examples/reference/steps/messages.korin"
```

The run:

--8<-- "docs-site/docs/generated/transcripts/reference/steps/messages.html"

Messages can use any scalar value. A secret cannot appear in a message.

## Durations

`poll` and `timeout` take durations: a whole number followed by `s`, `m` or `h`, such as
`30s`, `15m` or `2h`. A duration must be more than zero, and at most `720h` (30 days).
