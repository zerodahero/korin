# Hand off to an editor or TUI

Some steps need a person in a program: writing release notes in an editor, reviewing a diff
in a TUI, reading a document in a pager. `interactive` gives that program the terminal, and
waits for it to exit.

```korin title="notes.korin"
--8<-- "docs-site/examples/guides/handoff/notes.korin"
```

--8<-- "docs-site/docs/generated/transcripts/guides/handoff/notes.html"

## Getting results back

korin cannot read a program's screen. It sees two things: the exit code, and files the
program writes.

- **`{korin.out}`** is an empty, private directory for this step. Point the program at a file
  in it, then read the file with `capture <name> = file {korin.out}/<file> <mode>`. A file
  there is always new, so a result from an earlier run is never read by mistake.
- **`expect <name> = missing <path>`** handles the case where the person quit without
  saving. Without it, a missing file stops the wizard.
- **Any other path** works too, but the file must be new or changed by this step. An
  unchanged file counts as missing.

Many review tools can export what you did: comments, annotations, approvals. Have them write
to `{korin.out}`, and capture the file with `jq` if it is JSON.

## Ctrl-C and Ctrl-Z

While the program runs, it owns the terminal:

- **Ctrl-C** goes to the program, as it would in a shell. Many editors ignore it.
- **Ctrl-Z** suspends the program. korin takes the terminal back and asks whether to resume
  or kill it. Kill cancels the step and stops the wizard.

When the program exits, korin restores the terminal as it was before the handoff.

## Limits

- `interactive` takes no `timeout`: a person is working.
- A wizard with `interactive` cannot run [unattended](ci.md).
- korin cannot hide what the program shows, so `korin check` rejects secrets in its
  command. Pass them through `env` if the program needs them.
- An `interactive` step makes its stage mutating, because the program can change files.
