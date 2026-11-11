# tree-sitter-korin

A [tree-sitter](https://tree-sitter.github.io/) grammar for
[korin](https://github.com/zerodahero/korin) wizard and module files (`.korin`). It gives
editors syntax highlighting, folding, and jq and regex highlighting inside captures and
expects.

The reference parser is korin's
[`src/parser.rs`](https://github.com/zerodahero/korin/blob/main/src/parser.rs). This
grammar accepts every file that parser accepts. It is looser about rules that
`korin check` reports anyway, such as a header line after a stage or a `define` in a
wizard.

## Where this grammar lives

The grammar is developed in the korin repo, in
[`tree-sitter-korin/`](https://github.com/zerodahero/korin/tree/main/tree-sitter-korin),
so it changes in the same commit as korin's parser.
[zerodahero/tree-sitter-korin](https://github.com/zerodahero/tree-sitter-korin) is a
read-only mirror of that directory, updated by CI on every change and every korin release
tag. Editors install from the mirror, which holds only the grammar.

Open issues and pull requests on [korin](https://github.com/zerodahero/korin), not on the
mirror.

## Contents

| Path | What it is |
|---|---|
| `grammar.js` | The grammar. |
| `src/scanner.c` | The external scanner: indentation, and jq filters. |
| `src/parser.c`, `src/grammar.json`, `src/node-types.json` | Generated. Committed, so editors need no generator. |
| `queries/highlights.scm` | Highlights, with nvim-treesitter capture names. |
| `queries/injections.scm` | jq in capture filters, regex in `/.../` patterns. |
| `queries/folds.scm` | Folds for stages, templates, blocks and commands. |
| `test/corpus/`, `test/highlight/` | Tree and highlight tests. |

## Neovim

<!-- --8<-- [start:neovim] -->
This uses the `main` branch of nvim-treesitter.

```lua
vim.filetype.add({ extension = { korin = 'korin' } })

vim.api.nvim_create_autocmd('User', {
  pattern = 'TSUpdate',
  callback = function()
    require('nvim-treesitter.parsers').korin = {
      install_info = {
        url = 'https://github.com/zerodahero/tree-sitter-korin',
        -- Optional. A tag such as 'v0.1.0', or a full 40-character commit SHA. A short
        -- SHA fails: GitHub names the archive directory with the full SHA.
        revision = 'v0.1.2',
        queries = 'queries',
      },
    }
  end,
})
```

Then run `:TSInstall korin`. The jq and regex injections need the `jq` and `regex`
parsers too.
<!-- --8<-- [end:neovim] -->

## Helix

<!-- --8<-- [start:helix] -->
Add this to `languages.toml`:

```toml
[[language]]
name = "korin"
scope = "source.korin"
file-types = ["korin"]
comment-token = "#"
indent = { tab-width = 2, unit = "  " }

[[grammar]]
name = "korin"
source = { git = "https://github.com/zerodahero/tree-sitter-korin", rev = "<commit SHA>" }
```

Then run `hx --grammar fetch` and `hx --grammar build`. Copy `queries/` to
`~/.config/helix/runtime/queries/korin/`.
<!-- --8<-- [end:helix] -->

## Changing the grammar

In the korin repo, change the grammar in the same commit as `src/parser.rs`, then run:

```sh
mise run grammar
```

The task regenerates `src/`, runs the tests, and parses every `.korin` file in the repo. CI
runs it too, and fails if the committed `src/` differs from a fresh generation.
