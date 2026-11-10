# 3. Expects

Before it tags anything, the wizard should find out whether the version is already
released. The commands that tell you report "no" with a failing exit code. Normally, a
failing command stops the wizard. An **expect** says that a failure is an answer.

## Add a stage

Add a third stage at the end of the file:

```korin
--8<-- "docs-site/examples/tutorial/release-3.korin:stage"
```

??? note "The whole file"

    ```korin title="release.korin"
    --8<-- "docs-site/examples/tutorial/release-3.korin"
    ```

- **`expect untagged = exit 1`**: when `git rev-parse` exits 1, the step succeeds, and
  `untagged` becomes `true`. Any other failure still stops the wizard.
- **`expect unpushed = exit 2`**: `git ls-remote --exit-code` exits 2 when the remote has no
  such tag.
- **`when <condition>`** runs its indented body only when the condition is true. There is no
  `else`: write the opposite condition as another `when`.

A match can also test the output: `exit 1 and stderr ~ /not found/` holds only when both are
true. That is safer when an exit code can mean several things. See
[Expects](../reference/expects.md).

## Run it

Approve each stage, and answer `v0.2.0`.

--8<-- "docs-site/docs/generated/transcripts/tutorial/release-3.html"

Each command that matched shows `expect:` and the name.

## Captures and expects together

If a step has expects, its captures run only when none of them matched: there is no useful
output to read otherwise. `korin check` makes you show you know that. A capture from such a
step can be used only inside `when not <expect>`, for every expect of the step. See
[Using captures safely](../reference/expects.md#using-captures-safely).

Next: [Make changes](4-changes.md).
