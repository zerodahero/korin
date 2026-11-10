# Contributing to korin

## Setup

korin uses [mise](https://mise.jdx.dev) for every tool, including the Rust toolchain.

```sh
mise install
hk install        # installs the git hooks
mise run test
```

## Checks

`hk check --all` runs every linter that CI runs: rustfmt, clippy, cargo-deny, taplo, rumdl,
typos, shellcheck, shfmt, actionlint, zizmor, pinact and the denylist step. `hk fix --all`
applies the fixers.

## Keep examples generic

korin is a public tool. Examples, tests and docs use generic names only: `main-db-cluster`,
`example-org`, `123456789012`. Never use a real organization, account ID, hostname or
internal tool name.

The denylist step enforces this locally. It reads extended regexes, one per line, from
`~/.config/korin/denylist` (or the path in `KORIN_DENYLIST`). The file lives outside the
repo, so the patterns themselves are never committed. Seed it with the names you must not
leak. Without the file, the step warns and passes.

## Tests

- Parser and diagnostic tests use [insta](https://insta.rs) snapshots. Review new snapshots
  with `cargo insta review`.
- Runner tests put a fake `aws` from `tests/fixtures/` first on `PATH`. They never call a real
  cloud API.
- A syntax change also changes `tree-sitter-korin/grammar.js`. Run `mise run grammar` to
  regenerate it, run its tests and parse every `.korin` file. CI mirrors that directory to
  `zerodahero/tree-sitter-korin`; never edit the mirror directly.

## Docs site

The user docs live in `docs-site/`, built with [Zensical](https://zensical.org) and published
to GitHub Pages on every push to `main`.

```sh
mise run docs         # preview at http://localhost:8000, with live reload
mise run docs-build   # the strict build CI runs: broken links and missing snippets fail
```

Every example on the site is a real file under `docs-site/examples/`, pulled into a page with
a snippet (`--8<-- "docs-site/examples/..."`). Never paste an example into a page.

- **`tests/docs.rs`** runs `korin check` and `korin plan` on every example. Files under an
  `invalid/` directory are the exception: they show what `korin check` reports.
- **A transcript** is the terminal output of a run, shown on a page. To make one, add a
  sidecar `<name>.toml` beside `<name>.korin`. It lists the params and the answers to type at
  each prompt; the fields are documented on `Sidecar` in `tests/docs.rs`. The test runs the
  example in a pty, against the fake commands in `docs-site/examples/bin/`, and saves what
  the terminal shows as HTML under `docs-site/docs/generated/`.
- **After a change** to korin's output, an example, or a fake, regenerate and review:

  ```sh
  KORIN_UPDATE_DOCS=1 cargo nextest run --test docs
  git diff docs-site/docs/generated/
  ```

  Without `KORIN_UPDATE_DOCS`, the test fails when a saved transcript no longer matches.
- **A fake** handles only the exact commands the examples run, and fails on anything else, so
  a changed example cannot silently fall through to canned output.
- **A new feature** needs a reference page entry with at least two examples, and a
  transcript where the output shows something the text cannot.

## Commits

Use [Conventional Commits](https://www.conventionalcommits.org): `feat:`, `fix:`, `docs:`,
`test:`, `chore:`.
