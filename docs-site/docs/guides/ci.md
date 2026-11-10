# Run in CI

korin is built for a person at a terminal, approving each stage. Some wizards are safe to run
with no one watching, such as a nightly cleanup. The wizard and the run must both say so.

## Declare it in the wizard

```korin title="nightly.korin"
--8<-- "docs-site/examples/guides/ci/nightly.korin"
```

`unattended allowed` says the wizard may run without a terminal. Such a wizard cannot contain
`confirm` or `interactive`, which need a person. `korin check` enforces this.

## Approve in the run

```sh
korin run nightly.korin --yes
```

`--yes` approves every gate. korin logs each approval as a `gate_auto` event in the journal.

--8<-- "docs-site/docs/generated/transcripts/guides/ci/nightly.html"

Without a terminal there is no live view: each stage's preview stays in the log above its
output, and the gate line shows `approved by --yes`.

## What korin refuses

Without a terminal, korin checks before it runs anything:

- **No `--yes`**: it refuses, even for stages that only read.
- **No `unattended allowed`**: it refuses, even with `--yes`.
- **`--yes` with a terminal**: it refuses. At a terminal, read each gate yourself.
- **A missing value**: every `required` param and every `ask` must be given before stage 1,
  with `--param name=value`, or `KORIN_SECRET_<NAME>` for secrets.

--8<-- "docs-site/docs/generated/transcripts/guides/ci/no-yes.html"

## In GitHub Actions

```yaml
name: nightly
on:
  schedule:
    - cron: "0 3 * * *"
jobs:
  prune:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v5
      - uses: jdx/mise-action@v3   # installs korin from mise.toml
      - run: korin run ops/nightly.korin --yes
      - uses: actions/upload-artifact@v4
        if: always()
        with:
          name: korin-journal
          path: .korin/runs/
```

Keep the journal as an artifact. It has every command's full output, and the reason for a
failure.

## Wizards written by agents

See [Write wizards with an agent](agents.md) for the skill, the check loop, and how to review
what an agent wrote.
