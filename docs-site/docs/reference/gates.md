# Gates

A gate shows a whole stage and asks for approval before any of its commands run. A person
reads what will happen, with every value known so far, and answers `y` or `n`.

Any answer but `y` or `yes` stops the run. Nothing in the stage runs, and the run exits
with code 1.

```korin title="declined.korin"
--8<-- "docs-site/examples/reference/gates/declined.korin"
```

At the gate of stage 1:

--8<-- "docs-site/docs/generated/transcripts/reference/gates/declined.prompt-1.html"

The run, with stage 2 declined:

--8<-- "docs-site/docs/generated/transcripts/reference/gates/declined.html"

## What the gate shows

- **Every step of the stage**, numbered, with its kind: `read`, `mutate`, `note` and so on.
- **Values known from earlier stages**, filled in.
- **Values from inside the stage** as placeholders that name where they come from:
  `<current ← 1.1>` comes from step 1.1.
- **Secrets** as `«secret:name»`.
- **Steps inside `when`, `each` and `poll`**, and inside templates, even if they may not run.

## Answering a gate

One key answers a gate or a `confirm`; there is no need to press Enter.

- `y` approves.
- `n`, Enter or Esc declines. The default is no.
- `v` opens the [values view](#the-values-view), where you can see and change values.
- `l` hides or shows the legend of path prefixes, in the compact view. See
  [The legend](cli.md#the-legend).
- `f` switches between the compact and the full view, draws the preview again, and asks
  again. See [Compact view](cli.md#compact-view).
- Any other key, such as an arrow key, is ignored.

Keys typed before the question appears, or in the first quarter second after it, are
ignored. A key meant for something else cannot answer a gate you have not read. Ctrl-C still
cancels the run.

To answer with `y` and then Enter instead, pass `--require-enter` or set
`KORIN_REQUIRE_ENTER=1`.

## The values view

Press `v` at a gate, a `confirm` or a pause to list every value known so far: params, `ask`
answers, captures and expects from earlier stages. At a `confirm`, values made earlier in
the same stage are listed too. Values a later step will set are not listed, since that step
would replace them.

```korin title="values.korin"
--8<-- "docs-site/examples/reference/gates/values.korin"
```

--8<-- "docs-site/docs/generated/transcripts/reference/gates/values.prompt-2.html"

Type a value's number to change it, or press Enter to go back to the prompt. Then press `r`
to replace the value, `c` to clear an optional capture, or Enter to keep it. korin draws the
preview again with the new value:

--8<-- "docs-site/docs/generated/transcripts/reference/gates/values.prompt-6.html"

A new value must fit the value's kind:

| Kind | What to type |
|---|---|
| A param, an `ask`, a text or regex capture | One line. An empty line is the empty string. |
| A list (`lines`, `jq list`) | One item per line, then an empty line. Or one JSON array of strings, such as `["a", "b"]`; `[]` is the empty list. |
| JSON (`jq json`) | One line of JSON. |
| An expect | `y` or `n`. |
| A secret | Hidden input, as when korin asks for a secret. |

A secret or withheld value shows only as `«secret:name»` or `«withheld:name»`, never as its
text. You can still replace it.

At a `confirm`, the view also lists what the stage has found so far:

```korin title="values-confirm.korin"
--8<-- "docs-site/examples/reference/gates/values-confirm.korin"
```

--8<-- "docs-site/docs/generated/transcripts/reference/gates/values-confirm.prompt-3.html"

Each change is a `value_set` event in the [journal](journal.md), and a line in the report.

## `gates all`

The default. Every stage has a gate.

## `gates mutating`

Only mutating stages have a gate. A stage that only reads still shows its preview, then
pauses: Enter or Space runs it. With `--yes`, the pause continues on its own.

A stage is mutating if any step in it is `mutate`, `script mutate` or `interactive`. Steps
inside `when`, `each`, `poll` and templates count, whether or not they will run.

```korin title="mutating.korin"
--8<-- "docs-site/examples/reference/gates/mutating.korin"
```

--8<-- "docs-site/docs/generated/transcripts/reference/gates/mutating.html"

Stage 1 only reads, so it paused for Enter. Stage 2 changes the deployment, so it asked
first; the transcript shows the run after `y`.

## `mode guide`: no gates at all

A wizard with `mode guide` has no gates and shows no preview. It pauses before each stage,
and the stage runs when you press Enter or Space. A guide may only read: `korin check`
rejects `mutate` and `script mutate`, even inside a template. A `gates` line next to
`mode guide` is an error. See [Write a guide](../guides/walkthroughs.md).

## `korin plan`

`korin plan` prints every stage without running anything. It marks which stages are
mutating, which have a gate, and where a run can [start with `--from`](../guides/resume.md).

--8<-- "docs-site/docs/generated/transcripts/reference/gates/plan.html"

## `confirm`: a gate in the middle of a stage

A [`confirm`](steps.md#confirm) step asks again partway through a stage, once earlier steps
have found values worth checking. It shows the steps still to come.

## Steps run in order

korin never reorders steps or stages. What the gate shows is what runs, in that order.

## Without a terminal

A gate needs a person. Without a terminal, korin runs only a wizard that declares
`unattended allowed`, and only when you pass `--yes`, which approves every gate. See
[Run in CI](../guides/ci.md).
