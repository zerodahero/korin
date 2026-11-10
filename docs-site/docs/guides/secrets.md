# Keep secrets out of logs

A wizard often needs a token: to call an API, push to a registry, or log in to a database.
korin keeps such values out of the terminal, the journal and the report, as long as it knows
they are secret. This guide shows how to tell it.

## Mark the value as secret where it enters

A secret enters a wizard in one of three ways. Mark it at that point, and korin follows it
from there.

=== "A param"

    ```korin
    param db_password secret
    ```

    Give it in the environment, not on the command line:

    ```sh
    KORIN_SECRET_DB_PASSWORD="$(pass show shop/db)" korin run check-db.korin
    ```

    In a terminal, korin asks for it with hidden input when it is not set.

=== "An answer"

    ```korin
    ask secret token "GitHub token?"
    ```

    korin reads it with hidden input. Without a terminal, set `KORIN_SECRET_TOKEN`.

=== "A command's output"

    ```korin
    read aws ssm get-parameter --name /ci/deploy-bot-token --with-decryption --output json
      capture secret token = jq .Parameter.Value
    ```

    korin holds this command's output back until it knows the token, then shows it with the
    token hidden.

Anything built from a secret is secret too: a string or argument that uses `{token}`, an
`env` value, a template argument.

## Hand it to the command through `env`

```korin title="token.korin"
--8<-- "docs-site/examples/reference/secrets/token.korin"
```

--8<-- "docs-site/docs/generated/transcripts/reference/secrets/token.html"

`env GH_TOKEN={token}` puts the token in that one command's environment. Most CLIs read a
token from an environment variable: `GH_TOKEN`, `PGPASSWORD`, `VAULT_TOKEN`,
`AWS_SESSION_TOKEN`. Prefer that to an argument, which any user on the machine can read with
`ps` while the command runs.

When a CLI takes a token only as an argument, korin still hides it in everything it shows and
writes. It cannot hide it from `ps`.

## What korin refuses

`korin check` rejects a secret in a `note`, `warn`, `record` or `assert` message, and in an
`interactive` command, whose screen korin cannot see. A wizard cannot print a secret by
mistake.

## When output is withheld

If a secret-producing step fails to find its secret, korin cannot know what to hide. It then
withholds the whole output, and replaces it with a note of how many bytes it held back. Other
captures from that step become **withheld values**: usable in `when` and `assert`, never
shown, and never passed to a command.

See [Secrets](../reference/secrets.md) for every rule.
