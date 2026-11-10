# Journal and report

Every run writes a journal: a directory with what happened, in order. korin prints its path
when the run ends.

```text
.korin/runs/20260115T100000Z-tag-the-release/
  events.jsonl              one JSON event per line
  report.md                 a summary for people
  out/
    0001-s1.1.stdout        full output of each command
    0001-s1.1.stderr
    0003-s2.1/status        a copy of each file a capture read
```

- **Where:** `.korin/runs/` in the directory where `korin run` started.
- **Name:** the start time in UTC, then the title in lower case with dashes. If two runs
  start in the same second, the second one gets `-2`.
- **Redacted:** everything written here passes through the same redactor as the terminal.
  Secrets appear as `«secret:name»`.

Add `.korin/` to your `.gitignore`.

## An example

```korin title="release.korin"
--8<-- "docs-site/examples/reference/journal/release.korin"
```

--8<-- "docs-site/docs/generated/transcripts/reference/journal/release.html"

### report.md

```markdown
--8<-- "docs-site/docs/generated/transcripts/reference/journal/release.report.md"
```

The report lists approved gates, every `record` line, and how the run ended.

### events.jsonl

```json
--8<-- "docs-site/docs/generated/transcripts/reference/journal/release.events.jsonl"
```

Every event has `kind` and `ts`. The kinds:

| Kind | When |
|---|---|
| `run_start`, `run_end` | The run starts, and ends: `completed`, `failed` or `cancelled`. |
| `stage_start` | A stage starts. `mutating` says whether it changes anything. |
| `gate`, `gate_auto`, `confirm` | A gate or `confirm` is answered. `gate_auto` is an approval by `--yes`. |
| `command_start`, `command_end` | A command starts, and ends with its exit code or signal. |
| `expect` | An expect matched. |
| `capture` | A capture got a value. |
| `assert` | An `assert` was checked. |
| `say` | A `note`, `warn` or `record` was shown. |
| `handoff_start`, `handoff_end` | An `interactive` program took the terminal, and gave it back. |
| `escaped` | A cancelled command left a process running that korin could not stop. |

## Invocations

Each time a command runs, it gets an invocation ID. The same step can run many times, in an
`each` or a `poll`, and each run has its own ID.

`0042-s4.1/blue.1@7` reads as:

| Part | Meaning |
|---|---|
| `0042` | The 42nd command of the run. |
| `s4.1` | Step 4.1. |
| `/blue.1` | Step 1 of the template called as `blue`. |
| `[2]` | The item of an `each`, from 0, when there is one. |
| `@7` | The 7th attempt of a `poll`. |

Events about a command carry its `invocation_id`, and its output files use it as their name.

## Scratch directories

For each command, korin makes a private directory for [file captures](captures.md#file-captures),
`{korin.out}`, under the system's temporary directory. korin deletes it when the step ends.
If a run is killed before it can clean up, the next `korin run` removes directories left by
runs that are no longer alive.
