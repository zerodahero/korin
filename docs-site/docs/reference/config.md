# Config file

Your config file holds reader preferences: how korin shows a run to you. It never changes
what a wizard does.

korin reads `$XDG_CONFIG_HOME/korin/config.kdl`, or `~/.config/korin/config.kdl` when
`XDG_CONFIG_HOME` is not set. `KORIN_CONFIG=<path>` points at another file. A missing file
is fine: every key has a default.

There is no project config. A repository you clone cannot change how you see a gate.

## Format

The file is [KDL](https://kdl.dev) version 2. Each key is one line: the key, then its value.
The `open` key is a block instead, with one line inside for each kind it sets. `//` starts a
comment.

```kdl title="review.kdl"
--8<-- "docs-site/examples/reference/config/review.kdl"
```

```kdl title="plain.kdl"
--8<-- "docs-site/examples/reference/config/plain.kdl"
```

```kdl title="openers.kdl"
--8<-- "docs-site/examples/reference/config/openers.kdl"
```

## Keys

| Key | Values | Default | Also set by |
|---|---|---|---|
| `view` | `compact`, `full` | `compact` | – |
| `legend` | `#true`, `#false` | `#true` | – |
| `script-lines` | a whole number, 0 or more | `20` | `--script-lines`, `KORIN_SCRIPT_LINES` |
| `pager` | a command in quotes, such as `"less -R"` | `$PAGER`, else `less` | – |
| `require-enter` | `#true`, `#false` | `#false` | `--require-enter`, `KORIN_REQUIRE_ENTER` |
| `color` | `auto`, `always`, `never` | `auto` | `--color` |
| `icons` | `plain`, `nerd` | `plain` | – |
| `open` | a block: `script`, `json`, `markdown`, `text`, each with a command in quotes | the pager | `KORIN_OPEN_<KIND>` |

- **`view`** is the view the plan pane and the gates start in. See
  [Compact view](cli.md#compact-view). `f` still switches during a run. The run log, and a
  run without a terminal, always show the full view.
- **`legend`** shows the compact view's legend of path prefixes, at gates and in the plan
  pane. See
  [The legend](cli.md#the-legend). `l` still switches during a run.
- **`script-lines`** shows a script in full at its gate when it has at most this many lines.
  `0` never shows one. See [`script`](steps.md#what-the-gate-shows).
- **`pager`** is the program `o` opens a script or a folded output with, unless `open` names
  one for its kind. korin splits it into words, with no shell.
- **`require-enter`** makes gates and `confirm` read `y` and then Enter, instead of a
  single key.
- **`color`** works like `--color`. With `auto`, `NO_COLOR` still turns color off.
- **`icons`** picks the marks in the plan pane. `plain` uses `▶ ✓ ✗ ↷ ⚠`, which every font
  has. `nerd` uses Nerd Font glyphs, for a terminal set up with one.
- **`open`** names the program that `o` opens each kind of item with. See
  [Opening an item](cli.md#opening-an-item) for the kinds. A kind it leaves out opens in the
  pager. `KORIN_OPEN_JSON` (and the same for each kind) wins over the file. korin splits each
  command into words, with no shell.

A flag wins over the environment, the environment wins over the file, and the file wins
over the default. `KORIN_REQUIRE_ENTER=0` turns the key off, even when the file turns it on.

## Errors

An unknown key, a key set twice, or a value of the wrong kind is an error. korin names the
key and its line, and stops before it runs anything.

## `korin config`

```text
--8<-- "docs-site/docs/generated/help/config.txt"
```

`korin config` prints the file's path, then each key with its value and where the value
comes from: `flag`, `env`, `file` or `default`.

```text
/home/ada/.config/korin/config.kdl
view           full       file
legend         #true      default
script-lines   20         default
pager          $PAGER, else less default
require-enter  #true      file
color          auto       default
icons          plain      default
open.script    the pager  default
open.json      "jnv"      file
open.markdown  the pager  default
open.text      the pager  default
```
