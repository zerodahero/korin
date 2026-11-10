# 4. Make changes

Now the wizard can tag and push. These steps change things, so they get the most care.

## Mark what changes

Add `gates mutating` under the title:

```korin
--8<-- "docs-site/examples/tutorial/release-4.korin:gates"
```

Then add the last stage:

```korin
--8<-- "docs-site/examples/tutorial/release-4.korin:stage"
```

??? note "The whole file"

    ```korin title="release.korin"
    --8<-- "docs-site/examples/tutorial/release-4.korin"
    ```

- **`mutate`** runs a command that changes something. It works like `read`; the keyword
  tells korin, and you, what kind of step it is.
- **`gates mutating`** asks for approval only at stages that can change something. The first
  three stages only read, so they now run without asking. The `ask` still asks.
- **`confirm`** asks again, in the middle of a stage, right before the push. Answer anything
  but `y` and the wizard stops before it pushes.
- **`record`** prints a line like `note`, and also writes it to the run's report.

The `when` guards use the expects from stage 3. The tag is created only if it does not exist,
and pushed only if the remote does not have it.

## Run it

```sh
korin run ../release.korin
```

Answer `v0.2.0`. The gate of stage 4 shows every step that can run, including the steps
inside each `when`:

--8<-- "docs-site/docs/generated/transcripts/tutorial/release-4.prompt-2.html"

Approve the stage, and the push:

--8<-- "docs-site/docs/generated/transcripts/tutorial/release-4.html"

The report in the journal holds the gate and the `record` lines:

```markdown
--8<-- "docs-site/docs/generated/transcripts/tutorial/release-4.report.md"
```

## Run it again

A release can stop halfway: you answered `n` to the push, or the network failed. Run the
wizard again. Because each change is guarded by a check of the current state, it does only
what is left. Here the tag already exists locally, but was never pushed:

--8<-- "docs-site/docs/generated/transcripts/tutorial/release-4-again.html"

The tag step was skipped, and only the push ran. This is how korin resumes: it does not
replay old runs, it checks again. See [Resume after a stop](../guides/resume.md).

Next: [Reuse](5-reuse.md).
