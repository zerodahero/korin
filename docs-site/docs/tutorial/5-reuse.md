# 5. Reuse

The checks in stages 1 and 3 suit any repository. Move them into a module, so other wizards
can use them.

## Write a module

Create `~/korin-tutorial/lib/git.korin`:

```korin title="lib/git.korin"
--8<-- "docs-site/examples/tutorial/lib/git.korin"
```

- **`define`** names a template and its parameters. Its body is ordinary steps.
- **`exports`** lists what a caller can use. `untagged` and `unpushed` are expects, so the
  caller can use them in `when`.
- Inside the template, parameters work like params: `{tag}` in commands, `branch` in
  expressions.
- A module file holds only `use` and `define`, with no header.

## Use it

Replace the top of `release.korin`, through stage 1, with:

```korin
--8<-- "docs-site/examples/tutorial/release-5.korin:header"
```

Then replace stages 3 and 4:

```korin
--8<-- "docs-site/examples/tutorial/release-5.korin:released"
```

??? note "The whole file"

    ```korin title="release.korin"
    --8<-- "docs-site/examples/tutorial/release-5.korin"
    ```

- **`use "./lib/git.korin"`** loads the module, relative to `release.korin`. Its name is
  `git`, from the file name.
- **`git.clean_branch({branch})`** expands the template in place. A template call is not a
  black box: the gate and `korin plan` show every command inside it.
- **`as tag`** names this call, so its exports are `tag.untagged` and `tag.unpushed`.
- **`allow git`** lists the only program the wizard may run. `korin check` rejects any
  other, including inside templates. Add it when someone else, such as a coding agent,
  writes wizards for you to run.

## See the plan

`korin plan` shows each template call with the steps it expands to:

--8<-- "docs-site/docs/generated/transcripts/tutorial/release-5-plan.html"

Each stage is marked with whether it changes anything, and whether a run can start there.

## Start at a later stage

`korin run --from <stage>` skips the stages before it. korin allows it only when no later
step needs a value from a skipped stage. Here, stage 4 needs the expects from stage 3, so it
can't be a starting point:

```sh
korin check ../release.korin --from Release
```

--8<-- "docs-site/docs/generated/transcripts/tutorial/release-5-from.html"

Because every step checks the current state first, running the whole wizard again is safe,
and usually the simplest way to resume.

## What next

You have used most of korin. From here:

- [Keep secrets out of logs](../guides/secrets.md) when a wizard needs a token.
- [Run in CI](../guides/ci.md) for wizards that may run without a person.
- [Hand off to an editor or TUI](../guides/handoff.md) for review steps.
- The [Reference](../reference/steps.md) for every step and option.
