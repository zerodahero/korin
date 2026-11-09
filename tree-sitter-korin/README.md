# tree-sitter-korin

A [tree-sitter](https://tree-sitter.github.io/) grammar for korin wizard and module files
(`.korin`). It gives editors syntax highlighting, folding, and jq and regex highlighting
inside captures and expects.

The reference parser is `src/parser.rs`. This grammar accepts every file that parser
accepts. It is looser about rules that `korin check` reports anyway, such as a header line
after a stage or a `define` in a wizard.

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

This uses the `main` branch of nvim-treesitter.

```lua
vim.filetype.add({ extension = { korin = 'korin' } })

vim.api.nvim_create_autocmd('User', {
  pattern = 'TSUpdate',
  callback = function()
    require('nvim-treesitter.parsers').korin = {
      install_info = {
        url = 'https://github.com/zerodahero/korin',
        revision = '<commit or tag>',
        location = 'tree-sitter-korin',
        -- Relative to the repository root, not to `location`.
        queries = 'tree-sitter-korin/queries',
      },
    }
  end,
})
```

Then run `:TSInstall korin`. The jq and regex injections need the `jq` and `regex`
parsers too.

## Helix

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
source = { git = "https://github.com/zerodahero/korin", rev = "<commit>", subpath = "tree-sitter-korin" }
```

Then run `hx --grammar fetch` and `hx --grammar build`. Copy `queries/` to
`~/.config/helix/runtime/queries/korin/`.

## Changing the grammar

Change the grammar in the same commit as `src/parser.rs`, then run:

```sh
mise run grammar
```

The task regenerates `src/`, runs the tests, and parses every `.korin` file in the repo. CI
runs it too, and fails if the committed `src/` differs from a fresh generation.
