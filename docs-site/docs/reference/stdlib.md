# Standard library

korin has two built-in modules. Load them with `use aws` and `use rds`. Their templates call
the AWS CLI, which must be on `PATH` and logged in.

`korin modules` lists them:

```text
--8<-- "docs-site/docs/generated/modules.txt"
```

Each template is ordinary korin, shown in full below. Every command it runs appears at the
gate and in `korin plan`, under the call.

## `aws`

```korin title="aws.korin"
--8<-- "src/stdlib/aws.korin"
```

| Template | Exports |
|---|---|
| `aws.identity() as me` | `me.account`, `me.arn`: who the current credentials belong to. |
| `aws.ssm_get(name) as p` | `p.value`, and `p.missing`: true when the parameter does not exist. Use `p.value` inside `when not p.missing`. |
| `aws.ssm_put(name, value)` | Nothing. Writes a `String` parameter, replacing any value. |

## `rds`

```korin title="rds.korin"
--8<-- "src/stdlib/rds.korin"
```

| Template | Exports |
|---|---|
| `rds.cluster(id) as c` | `c.status`, `c.arn`, `c.version`, `c.endpoint`, `c.writer` (optional), `c.security_groups` (a list). |
| `rds.instance(id) as i` | `i.status`, `i.version`, `i.endpoint` (optional). |
| `rds.await_cluster(id, status)` | Nothing. Polls every 30s for up to 2h until the cluster has the status. Stops early on a status it cannot recover from. |
| `rds.await_snapshot(id)` | Nothing. Polls every 30s for up to 2h until the cluster snapshot is available. |
| `rds.delete_cluster(id)` | Nothing. Deletes the cluster without a final snapshot. |

## Your own modules

The standard library is small on purpose. Write templates for your own tools in a module
file next to your wizards. See [Templates and modules](templates.md).
