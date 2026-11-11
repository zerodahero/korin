# Write a guide

Some runbooks change nothing. They walk a person through a system: where the code lives,
how the service is configured, what the logs say right now. Asking `y/N` before each stage
of such a tour adds nothing, because nothing in it changes state.

`mode guide` makes that kind of wizard. A guide shows no preview and has no gates. It
pauses before each stage, and the stage runs when you press Enter or Space. Each command
still shows as it runs, with its output under it.

```korin title="tour.korin"
--8<-- "docs-site/examples/guides/walkthroughs/tour.korin"
```

--8<-- "docs-site/docs/generated/transcripts/guides/walkthroughs/tour.html"

## A guide only reads

A guide never asks before a command runs, so it may only run commands that read:

- **Allowed:** `read`, `script read`, `interactive`, `note`, `warn`, `record`, `ask`,
  `confirm`, `when`, `each`, `poll`, and templates made of these.
- **Rejected by `korin check`:** `mutate` and `script mutate`, also inside a template. A
  template that changes something is reported at its call, with the step that changes it.
- **Rejected:** a `gates` line. A guide has no gates.

korin trusts `read` the way it does in any wizard: a `read` step is one the author says
changes nothing. Keep each `read` honest, because no gate stands before it.

## Pauses

A guide stops for the person in three places:

- **Before each stage:** `↵ continue · f full`. Enter or Space runs the stage. `f` switches
  between the [compact and the full view](../reference/cli.md#compact-view). Ctrl-C cancels
  the run. Other keys are ignored.
- **Before an `interactive` step:** `↵ open · Ctrl-C cancel`. The command shows above the
  pause, so the person knows which program takes the terminal.
- **At a `confirm`:** it asks `y/N`, as in any wizard.

With `--require-enter`, a pause takes a whole line: an empty line continues, and `f` then
Enter switches the view.

Without a terminal, a guide runs like any wizard: it needs `unattended allowed`, and `--yes`
passes every pause. The journal records each pause as a `pause` event.

## Write for the reader

A guide has no preview, so the notes carry the story. Say what each stage shows before it
runs, and what the person should notice in the output after it. A `record` puts a finding in
the run's report.

`korin plan` shows a guide's stages with `pause` in place of a gate:

--8<-- "docs-site/docs/generated/transcripts/guides/walkthroughs/tour-plan.html"
