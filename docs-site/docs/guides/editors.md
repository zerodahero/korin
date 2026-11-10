# Editor support

The [tree-sitter-korin](https://github.com/zerodahero/tree-sitter-korin) grammar gives editors
syntax highlighting and folding for `.korin` files, plus jq and regex highlighting inside
captures and expects.

## Neovim

--8<-- "tree-sitter-korin/README.md:neovim"

### Format with conform.nvim

[conform.nvim](https://github.com/stevearc/conform.nvim) can run `korin fmt` on save.
`korin fmt` rewrites a file in place and does not read stdin, so the formatter sets
`stdin = false`. conform then writes the buffer to a temporary file beside the original,
formats it, and reads it back.

```lua
require('conform').setup({
  formatters = {
    korin_fmt = {
      command = 'korin',
      args = { 'fmt', '$FILENAME' },
      stdin = false,
    },
  },
  formatters_by_ft = {
    korin = { 'korin_fmt' },
  },
  format_on_save = { timeout_ms = 500 },
})
```

The `korin` filetype comes from the `vim.filetype.add` line above. When the file does not
parse, `korin fmt` exits 1 and leaves it unchanged; conform reports the error. Run `:ConformInfo`
to see the formatter's log.

## Helix

--8<-- "tree-sitter-korin/README.md:helix"

## Other editors

Any editor with tree-sitter support can use the grammar. The queries in `queries/` use
nvim-treesitter's capture names.

## Checks as you type

There is no language server yet. Run `korin check` from your editor, or on save. It exits 1
on any problem, and `--format json` gives the file, line and column of each one.
