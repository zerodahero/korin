# Your first wizard

A wizard is a `.korin` file: a list of commands, grouped into stages. korin shows each stage
before it runs, and waits for you to approve it.

This first wizard tidies a git repository: it finds branches already merged into `main`, and
deletes them. You can run it in any repository.

## Write it

Save this as `tidy.korin`, next to a repository rather than in it:

```korin title="tidy.korin"
--8<-- "docs-site/examples/get-started/tidy.korin"
```

- **`korin 1`** and **`title`** start every wizard.
- **`param base = "main"`** is a value you can change when you run it:
  `--param base=trunk`.
- **`stage`** groups steps. Each stage asks before it runs.
- **`read`** runs a command that changes nothing. **`mutate`** runs one that changes
  something.
- **`capture merged = lines`** keeps the command's output as a list, one item per line.
- **`each`** repeats its body once per item, and **`when`** skips items that don't match.

## Check it

```sh
korin check tidy.korin
```

`korin check` finds mistakes, such as a misspelled name or a list used where a single value
belongs, without running anything.

## Run it

From inside the repository:

```sh
korin run ../tidy.korin
```

korin shows stage 1 and asks `Run stage 1? [y/N]`. Press `y`. It runs
`git branch --merged`, and notes each merged branch. Then it shows stage 2, with what it
will do:

--8<-- "docs-site/docs/generated/transcripts/get-started/tidy.prompt-2.html"

`<branch ← 2.1>` is a value korin knows only once the step runs: each branch, in turn. Type
`y` again:

--8<-- "docs-site/docs/generated/transcripts/get-started/tidy.html"

Answer anything but `y` at a gate, and the run stops before that stage does anything.

The `journal:` line points to a record of the run. korin keeps one for every run, in
`.korin/runs/`. Add `.korin/` to the repository's `.gitignore`.

## Where next

- The [tutorial](../tutorial/index.md) builds a release wizard step by step: values,
  expected failures, changes, and reuse.
- The [reference](../reference/steps.md) lists every step and option.
