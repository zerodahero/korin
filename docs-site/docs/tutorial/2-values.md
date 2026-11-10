# 2. Values

The wizard needs a version number. It should look at the last release, show what changed
since, and ask for the new version.

## Add a param

Add a param under the title:

```korin
--8<-- "docs-site/examples/tutorial/release-2.korin:param"
```

A **param** is a value given when the run starts. This one has a default, so it is optional.
Give another value with `--param remote=upstream`. Use it as `{remote}`.

## Add a stage

Add a second stage at the end of the file:

```korin
--8<-- "docs-site/examples/tutorial/release-2.korin:stage"
```

??? note "The whole file"

    ```korin title="release.korin"
    --8<-- "docs-site/examples/tutorial/release-2.korin"
    ```

This stage uses three more ways to capture a value:

| Capture | Gives |
|---|---|
| `regex /refs\/tags\/(v[0-9]+\.[0-9]+\.[0-9]+)/` | The part in parentheses, from the first match. The remote lists the newest version first, so this is the last release. A `/` inside the regex is written `\/`. |
| `text` | The whole output, trimmed: here, the number of new commits. |
| `lines` | A list, one item per line: here, one per commit. |

And three more steps:

- **`each commit in commits`** runs its indented body once per item of the list.
- **`ask version "..."`** asks you for a value while the wizard runs.
- **`assert version ~ /.../`** checks the answer against a regex. `ahead > 0` compares
  numbers.

`{last}` in a command is one argument, whatever it holds. In `git log --oneline
{last}..HEAD`, the text around it stays in the same argument: `v0.1.0..HEAD`.

## Run it

```sh
korin run ../release.korin
```

Approve both stages, and answer `v0.2.0` when asked.

--8<-- "docs-site/docs/generated/transcripts/tutorial/release-2.html"

## Plan without running

At the gate of stage 2, korin can't show `last` yet: it comes from a command in the same
stage. It shows where the value will come from instead, as `<last ← 2.1>`. `korin plan`
prints the whole wizard that way, without running anything:

```sh
korin plan ../release.korin
```

Next: [Expects](3-expects.md).
