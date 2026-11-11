# File structure

A wizard is a `.korin` file. It has a header, then imports, then params, then stages. Blocks
are marked by indentation.

```korin title="header.korin"
--8<-- "docs-site/examples/reference/file/header.korin"
```

## Lines and indentation

- **One statement per line.** A step's options, and the body of `when`, `each` and `poll`,
  are indented under it.
- **Indent with spaces.** A tab is an error. Any consistent width works; `korin fmt` uses two
  spaces.
- **Comments** start with `#` at the start of a line or after a space, and run to the end of
  the line. A `#` inside a word, such as `page#top`, is text.

## Header

| Line | Required | Meaning |
|---|---|---|
| `korin 1` | Yes, first line | The language version. |
| `title "<text>"` | Yes | Shown at the start of the run, and used to name the run's [journal](journal.md) directory. |
| `gates all\|mutating` | No, default `all` | Which stages ask for approval. See [Gates](gates.md). |
| `mode stepwise\|guide` | No, default `stepwise` | `guide` makes a read-only walkthrough with no gates. See [Write a guide](../guides/walkthroughs.md). |
| `unattended allowed` | No | Lets the wizard run without a terminal. See [Run in CI](../guides/ci.md). |
| `allow <name-or-path>, ...` | No | The only executables the wizard may run. |

Each header line may appear only once.

### `allow`

`allow` lists every executable the wizard may start. `korin check` rejects any command whose
executable is not listed, including commands inside templates. A bare name, such as
`kubectl`, matches that name on `PATH`. A path matches the file it points to.

Use `allow` when someone else wrote the wizard, such as a coding agent. One line tells the
reader everything it can run.

## Imports

```korin
use aws                       # a module built into korin
use "./lib/k8s.korin"         # a module file, relative to this file
use "./lib/k8s.korin" as kube # the same, called `kube` instead of `k8s`
```

A module's name is its file name without `.korin`, unless `as` renames it. See
[Templates and modules](templates.md).

## Params

A param is a value given when the run starts.

```korin
param tag required                              # must be given
param namespace = "production"                  # has a default
param image = "registry.example.com/web:{tag}"  # a default can use earlier params
param registry_token secret                     # asked for with hidden input
```

- Give values with `--param name=value`. Repeat it for each param.
- A `required` param with no value is asked for at the start of the run, in a terminal.
- A `secret` param is never taken from the command line. korin reads it from
  `KORIN_SECRET_<NAME>` (here `KORIN_SECRET_REGISTRY_TOKEN`), or asks for it with hidden
  input. See [Secrets](secrets.md).

## Stages

`stage "<title>"` starts a stage. Its steps follow, indented. A stage must have at least one
step. Every stage is shown in full before it runs, and asks for approval at its
[gate](gates.md).

Stages are numbered from 1. Steps are numbered within their stage: `2.3` is the third step of
stage 2, and `2.3.1` is the first step inside it.

## Module files

A module file holds only `use` and `define` lines, and no header. See
[Templates and modules](templates.md).

```korin title="lib/k8s.korin"
--8<-- "docs-site/examples/reference/templates/lib/k8s.korin"
```

## Commands and arguments

korin never uses a shell. It splits each command line into words on spaces, and each word
becomes one argument.

```korin title="arguments.korin"
--8<-- "docs-site/examples/reference/file/arguments.korin"
```

`korin plan` shows the arguments each command will get, quoted where needed:

--8<-- "docs-site/docs/generated/transcripts/reference/file/arguments.html"

The rules:

- **`{name}` inside a word inserts a value.** The result is still one argument, even if the
  value holds spaces, quotes or `$(...)`. Nothing in a value is ever run.
- **`{list...}` inserts a list** as one argument per item. It must be a whole word.
- **Double quotes** make one argument from text with spaces: `"fix: retry"`.
- **A backslash** keeps the next character literal: `\{`, `\}`, `\"`, `\\`, `\#`, `\,`
  and `\)`, and a backslash before a space keeps the space in the word. `\n` and `\t` are a
  newline and a tab.
- **Pipes, redirects, globs, `~` and `$VAR`** have no special meaning. `*.log` is passed as
  the text `*.log`. Use a [jq capture](captures.md) or a [`script`](steps.md#script) for
  work that needs a shell.

### The executable

The first word is the executable. It must be known before the run starts, so it cannot come
from a param, an `ask` or a capture. It can be literal text, or a template parameter given
literal text.

korin finds every executable before stage 1:

- A bare name, such as `kubectl`, is looked up on `PATH`.
- An absolute path is used as is.
- A relative path, such as `./scripts/fix.sh`, resolves against the directory of the `.korin`
  file that holds the step.

If one is missing or not executable, the run stops before any command runs. A later change to
`PATH` cannot swap a program mid-run.

### Where commands run

Commands run in the directory where you started `korin run`. Their standard input is
`/dev/null`, so a command that waits for input gets end-of-file at once. Use
[`interactive`](steps.md#interactive) for programs that need the terminal.

## Formatting

`korin fmt <files>` rewrites files in the standard layout. It indents two spaces per level,
trims trailing spaces, keeps at most one blank line in a row, puts a blank line before each
`stage` and `define`, and aligns the `=` of consecutive `capture` lines. Comments are kept. `korin fmt --check <files>` lists files that are not formatted,
and changes nothing.
