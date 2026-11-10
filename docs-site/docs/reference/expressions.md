# Expressions

Expressions appear in `when`, `assert`, and a poll's `until` and `fail`. They are small on
purpose: comparison and logic, no arithmetic. Anything more goes through a
[jq capture](captures.md) or a [`script`](steps.md#script).

```korin title="rules.korin"
--8<-- "docs-site/examples/reference/expressions/rules.korin"
```

--8<-- "docs-site/docs/generated/transcripts/reference/expressions/rules.html"

## Operands

- **Names:** params, captures, `ask` values, and template exports such as `web.ready`. Write
  them bare: `replicas`, not `{replicas}`.
- **Expects:** an expect's name is `true` when it matched.
- **Literals:** strings in double quotes (`"OPEN"`), numbers (`2`, `0.5`), and lists of
  strings (`["staging", "production"]`).

## Operators

| Operator | Example | Meaning |
|---|---|---|
| `==`, `!=` | `state == "OPEN"` | Compare text. `2` and `"2"` are equal. |
| `<`, `<=`, `>`, `>=` | `replicas >= 2` | Compare numbers. A value that is not a number stops the wizard. |
| `in` | `env in ["staging", "production"]` | The value is an item of the list. The list can also be a list capture. |
| `~` | `image ~ /:latest$/` | The value matches the regex. |
| `exists` | `exists owner` | An [optional capture](captures.md#optional-captures) has a value. |
| `and`, `or`, `not` | `not (a or b)` | Logic. Use parentheses to group. |

`not` binds tighter than `and`, and `and` tighter than `or`. When in doubt, add parentheses.

## Values and their text

Every value compares by its text. A number from a jq capture is in jq's canonical form, so
`.replicas` gives `4`, and a boolean gives `true` or `false`. Compare booleans with
`draft == "false"`. A `jq json` capture compares as compact JSON text.
