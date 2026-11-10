---
hide:
  - toc
---

# korin

**Guided runbooks for commands that need a human watching.**

Some work is a long series of commands where a mistake is costly: a database migration, a
cluster upgrade, a release, a cleanup in production. You want each command written down,
reviewed, and run in order, with a person approving each step and a record of what happened.

korin runs `.korin` files that describe that work. Every command is shown before it runs.
Every stage waits for approval. Values found by one step, such as an ID or a status, flow into
the next, and korin checks the whole file before anything runs.

```korin title="roll.korin"
--8<-- "docs-site/examples/reference/gates/mutating.korin"
```

--8<-- "docs-site/docs/generated/transcripts/reference/gates/mutating.html"

## What it does

- **Shows before it runs.** Each stage is printed in full, with every value known so far,
  and waits at a [gate](reference/gates.md).
- **Checks before it starts.** [`korin check`](reference/cli.md#korin-check) catches unknown
  names, misused values and unguarded captures, before any command runs.
- **No shell.** Each command is a list of arguments. A value is always one argument, and
  nothing in it is ever run.
- **Typed captures.** Take values from JSON with jq, from text with a regex, or line by line.
  An [expect](reference/expects.md) turns a known failure, like "not found", into an answer.
- **Safe to run again.** Guard each change with a check of the current state, and a stopped
  run [resumes](guides/resume.md) by running again.
- **Keeps secrets secret.** Tokens are [hidden](reference/secrets.md) in the terminal, the
  journal and the report.
- **A journal of every run**: each command, its full output, and each answer.

## Start here

- [Install](get-started/install.md) korin.
- Write [your first wizard](get-started/first-wizard.md).
- Follow the [tutorial](tutorial/index.md) to build a release wizard.
