# Editor support

The [tree-sitter-korin](https://github.com/zerodahero/tree-sitter-korin) grammar gives editors
syntax highlighting and folding for `.korin` files, plus jq and regex highlighting inside
captures and expects.

## Neovim

--8<-- "tree-sitter-korin/README.md:neovim"

## Helix

--8<-- "tree-sitter-korin/README.md:helix"

## Other editors

Any editor with tree-sitter support can use the grammar. The queries in `queries/` use
nvim-treesitter's capture names.

## Checks as you type

There is no language server yet. Run `korin check` from your editor, or on save. It exits 1
on any problem, and `--format json` gives the file, line and column of each one.
