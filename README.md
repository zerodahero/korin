# Korin

A guided runbook language for commands that need human oversight.

A korin wizard is a `.korin` file: a series of stages, each made of commands. korin parses
the whole file before anything runs, so it knows every command and every value that flows
from one command into the next. It shows each stage before running it, waits for your
approval, and stops at the first surprise.

```text
stage "Snapshot"
  mutate aws rds create-db-cluster-snapshot --db-cluster-identifier {cluster} --db-cluster-snapshot-identifier {scratch}-snap --output json
    capture snap_arn = jq .DBClusterSnapshot.DBClusterSnapshotArn
  mutate aws ssm put-parameter --name /rehearsal/{cluster}/snapshot --value {snap_arn} --overwrite
```

At the gate, korin shows that the second command depends on the first:

```text
━━ Stage 3/5 · Snapshot ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

  3.1  mutate ❯ aws rds create-db-cluster-snapshot \
                  --db-cluster-identifier main-db-cluster \
                  --db-cluster-snapshot-identifier main-db-cluster-rehearsal-snap \
                  --output json
        → snap_arn
  3.2  mutate ❯ aws ssm put-parameter \
                  --name /rehearsal/main-db-cluster/snapshot \
                  --value <snap_arn ← 3.1> \
                  --overwrite
  ? Run stage 3? [y/N]
```

> korin is under active development. The language is not stable yet.

## Install

```sh
mise use github:zerodahero/korin
```

## Use

```sh
korin check wizard.korin     # parse and lint, without running anything
korin plan wizard.korin      # print every stage and command
korin run wizard.korin --param cluster=main-db-cluster
korin fmt wizard.korin       # canonical layout; --check to verify only
korin modules                # list the built-in aws and rds templates
```

## Write wizards with an agent

The korin skill teaches coding agents to write and check wizards. In Claude Code:

```text
/plugin install korin --marketplace zerodahero/korin
```

Other agents can copy [`skills/korin/`](skills/korin/). See
[Write wizards with an agent](https://zerodahero.github.io/korin/guides/agents/).

## Documentation

**<https://zerodahero.github.io/korin/>**: install, a tutorial, guides, and the full
language reference. The site's source is in [`docs-site/`](docs-site/).

- [Editor support: tree-sitter grammar](tree-sitter-korin/README.md)
- [Changelog](CHANGELOG.md)
- [Contributing](CONTRIBUTING.md)
- [Security policy](SECURITY.md)

## License

Apache-2.0. See [LICENSE](LICENSE).

<!-- Last, very bottom -->
### Why "Korin"?

¯\\\_(ツ)\_/¯

I created a bunch of wizard fantasy names with <https://reedsy.com/studio/generators/fantasy-name/wizard/>, and "Korin Amaru: Given name means 'walker of the first paths,' a guide whose counsel reaches back to the world's dawn." seemed like as good a fit as any.
