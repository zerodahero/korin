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

## Commits

Use [Conventional Commits](https://www.conventionalcommits.org): `feat:`, `fix:`, `docs:`,
`test:`, `chore:`.
