# Resume after a stop

Runs stop: a command fails, a gate is declined, the network drops, someone presses Ctrl-C.
korin does not replay a run from its journal. Instead, write the wizard so that running it
again is safe. Then the answer to "where was I?" is always: run it again.

## Check before each change

Guard every change with a check of the current state. If the change is already done, the
check finds it, and the change is skipped.

```korin title="not-found.korin"
--8<-- "docs-site/examples/reference/expects/not-found.korin"
```

The first run finds no release, and creates it. A second run finds it, and only says so. The
pattern is always the same:

1. A `read` that answers "is it done?", with an [expect](../reference/expects.md) for the
   "no" answer.
2. `when <not done>` around the change.

The [tutorial](../tutorial/4-changes.md#run-it-again) shows a release that stopped between
the tag and the push. The second run skips the tag and pushes.

## A cancelled change

When korin cancels a `mutate`, by Ctrl-C or a timeout, the stop message says the change may
be partially applied. A guarded wizard handles that too: the next run checks again.

## Start at a later stage

`korin run --from <stage>` starts at a later stage, and takes the stages before it as done.
Give the stage's title or number.

```sh
korin run mutating.korin --param image=registry.example.com/api:2.4.0 --from Roll
```

--8<-- "docs-site/docs/generated/transcripts/reference/gates/from.html"

A [preflight](../reference/file.md#preflight) runs first, even with `--from`. Put the lookups
every run needs there, and a later start still has their values.

A later step may read a value made before the start: a capture, an expect, a template export
or an `ask` answer. Params are fine, because you give them again. For the others:

- **In a terminal,** korin lists them after the preflight, and you set each one by hand, or
  skip the stages that read them. See [Skipping a stage](../reference/gates.md#skipping-a-stage).
- **Without a terminal,** give each with `--set name=value` (or `--param` for an `ask`).
  Otherwise korin refuses before anything runs.

`korin plan` lists, for each stage, what a start there asks for. `korin check --from <stage>`
warns about each value.

Typing old values back is easy to get wrong. If the wizard checks before each change, running
it whole is safe, and the earlier stages only read.
