# Templates and modules

A template is a named group of steps that a wizard can use many times. A module is a file of
templates. korin expands every template call in place before it plans anything, so the gate
and `korin plan` show each command a template runs.

## Define a template

A module file holds `use` and `define` lines only.

```korin title="lib/k8s.korin"
--8<-- "docs-site/examples/reference/templates/lib/k8s.korin"
```

```text
define <name>(<param>, ...)
  exports <name>, ...      # optional; must be the first line of the body
  <step>
  ...
```

- **Parameters** are positional. Inside the body, use them like params: `{name}` in
  commands and messages, `name` in expressions.
- **The body sees** its parameters, its own captures and expects, and other templates. It
  does not see the caller's names.
- **A template holds steps**, never stages.
- **Templates can call templates**, but not themselves, directly or through others.
  `korin check` prints the chain of calls if they loop.

## Call a template

```korin title="restart.korin"
--8<-- "docs-site/examples/reference/templates/restart.korin"
```

--8<-- "docs-site/docs/generated/transcripts/reference/templates/restart.html"

`<module>.<template>(<args>) [as <alias>]`

- **Each argument is one word**, like an argument in a command. `{scratch}-snap` is one
  argument; `web` is the literal text `web`.
- **A list argument** passes a list: give a list's name, such as `({pods})`.
- **Secret arguments stay secret** inside the template.
- **Steps from a template** are numbered under the call: `2.1.1` and `2.1.2` are the two
  steps of `k8s.restart` at step `2.1`.

### Exports

`exports` lists the captures and expects the caller may use, as `<alias>.<name>`. Anything
not exported is private to the call.

`as <alias>` is required when the template exports anything. An alias must be unique where it
is used. Each call has its own names, so two calls never collide: `web.ready` and `api.ready`
above come from two runs of the same `read`.

An exported capture keeps its type, and an optional capture stays optional. An exported
expect is `true` when it matched, so a caller can write `when not prev.missing`.

## Modules

| Line | Loads |
|---|---|
| `use aws` | A module built into korin. See [Standard library](stdlib.md). |
| `use "./lib/k8s.korin"` | A module file, relative to the file that holds the `use`. |
| `use "./lib/k8s.korin" as kube` | The same module, called `kube`. |

- A module's name is its file name without `.korin`, unless `as` renames it.
- A module can `use` other modules. A module loaded twice is read once.
- `korin check` rejects a cycle of imports, and prints the chain.

### The standard library

```korin title="stdlib.korin"
--8<-- "docs-site/examples/reference/templates/stdlib.korin"
```

--8<-- "docs-site/docs/generated/transcripts/reference/templates/stdlib.html"

`korin modules` lists every built-in template and what it exports.
