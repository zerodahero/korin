# 1. Look before you change

A release should start from a known state: the `main` branch, with nothing uncommitted. The
first version of the wizard only checks that, and changes nothing.

## Write the wizard

Create `~/korin-tutorial/release.korin`:

```korin title="release.korin"
--8<-- "docs-site/examples/tutorial/release-1.korin"
```

Line by line:

- **`korin 1`** names the language version. It is always the first line.
- **`title`** names the wizard. korin shows it at the start of the run.
- **`stage`** starts a group of steps. korin shows each stage in full, and asks before it
  runs any of it.
- **`read`** runs a command that changes nothing. The rest of the line is the command,
  split into words. There is no shell, so nothing in it is expanded or run except the
  command itself.
- **`capture branch = text`** keeps the command's output, trimmed, as `branch`. It is
  indented under the step it belongs to.
- **`assert`** stops the wizard with the message if the condition is false. `{branch}`
  inserts the value into the message.
- **`note`** prints a line.

## Check it

`korin check` finds mistakes without running anything:

```sh
korin check ../release.korin
```

```text
✓ ../release.korin is valid
```

Try changing `{branch}` in the message to `{current}`, a name nothing defines, and run
`korin check` again. It points at the unknown name, and says how to define it.

## Run it

```sh
korin run ../release.korin
```

korin shows the whole stage and asks `Run stage 1? [y/N]`. This is a **gate**: the moment
to read what will run before anything does. Type `y` and press Enter.

--8<-- "docs-site/docs/generated/transcripts/tutorial/release-1.html"

Once you approve, korin erases the preview and shows each command with its output under it.
Steps without output, like the `assert`s, run quietly.

## Watch it stop

Change a file, so the working tree is not clean, and run the wizard again:

```sh
echo "total = 1" >> cart.txt
korin run ../release.korin
```

--8<-- "docs-site/docs/generated/transcripts/tutorial/release-1-dirty.html"

The `assert` stopped the run with exit code 1, and showed what `git status` found. Undo the
change before you go on:

```sh
git checkout cart.txt
```

## The journal

Each run ends with a `journal:` line. korin writes a record of every run to `.korin/runs/`
in the directory you ran it in: each command, its full output, and each answer you gave. The
setup script added `.korin/` to the repository's `.gitignore`, so the journal never makes the
tree dirty. Do the same in your own repositories. See [Journal](../reference/journal.md).

Next: [Values](2-values.md).
