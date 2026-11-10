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

## `gates all`

The default. Every stage has a gate.

## `gates mutating`

Only mutating stages have a gate. Other stages are shown, then run without asking.

A stage is mutating if any step in it is `mutate`, `script mutate` or `interactive`. Steps
inside `when`, `each`, `poll` and templates count, whether or not they will run.

```korin title="mutating.korin"
--8<-- "docs-site/examples/reference/gates/mutating.korin"
```

--8<-- "docs-site/docs/generated/transcripts/reference/gates/mutating.html"

Stage 1 only reads, so it ran without asking. Stage 2 changes the deployment, so it asked
first; the transcript shows the run after `y`.

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
