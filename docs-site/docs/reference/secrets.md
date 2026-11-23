# Secrets

A secret is a value korin never shows or writes down. In its place, the terminal, the
journal and the report show `«secret:name»`.

## Secrets in the run log

A command's `$` line names an exact secret value as a braced shell variable, so you can paste
the line into a shell:

```text
$ GH_TOKEN="${TOKEN}" gh workflow run deploy.yml --ref main
  ↳ uses ${TOKEN} (secret token)
```

The variable is the secret's name in capitals, with every other character as `_`. Each value
gets its own variable: the items of a secret list are `TOKENS_1`, `TOKENS_2`, and a secret
captured again with a new value gets the next free name, such as `TOKEN_2`. The first line
that uses a variable says which secret it holds. korin never sets these variables for you:
set them yourself before you paste a line.

A line that holds only part of a secret, such as one field of a JSON secret, shows that part
as `«secret:name»`, and says it cannot be pasted.

## Where secrets come from

| Source | Example |
|---|---|
| A secret param | `param db_password secret` |
| A secret `ask` | `ask secret token "GitHub token?"` |
| A secret capture | `capture secret token = jq .Parameter.Value` |
| Anything built from one | A word, a string, a template argument or an `env` value that uses `{token}` |

A secret param is never read from the command line. Give it in the environment as
`KORIN_SECRET_<NAME>`, such as `KORIN_SECRET_DB_PASSWORD`, or type it when korin asks.

## Hidden input

korin asks for a secret with hidden input: nothing you type appears. The prompt shows only
a count of the characters so far, such as `token (hidden): · 52 chars`, so you can tell that
a paste arrived whole. This helps with a password manager whose clipboard empties after a
few seconds.

- Backspace removes the last character, and Ctrl-U clears the line.
- Enter submits. The count then becomes `✓`, so the length does not stay in the
  scrollback. The journal and the report never record it.
- Ctrl-C cancels the run.

A secret must be at least 4 characters long. Shorter values would match too much ordinary
text.

## Pass secrets with `env`

```korin title="param.korin"
--8<-- "docs-site/examples/reference/secrets/param.korin"
```

`env NAME={secret}` gives the secret to one command through its environment. A secret can
also appear in a command's arguments, but any user on the same machine can read arguments
with `ps`. Commands never see the `KORIN_SECRET_*` variables themselves.

A secret cannot appear in a `note`, `warn`, `record` or `assert` message, or in an
`interactive` command. `korin check` rejects these.

## Secret captures

```korin title="token.korin"
--8<-- "docs-site/examples/reference/secrets/token.korin"
```

--8<-- "docs-site/docs/generated/transcripts/reference/secrets/token.html"

A step with a `capture secret` is a **secret-producing step**, and korin treats its output
with extra care:

- **Its output is held** in memory and not shown while the command runs.
- **It is published only when every secret capture of the step got a value.** Then korin
  knows each secret, and hides it everywhere, as the token above.
- **Otherwise it is withheld.** If an expect matched, a secret capture was absent, or a
  capture failed, korin cannot know what to hide. The output is replaced by a note such as
  `withheld: 812 bytes; unresolved secrets: token`.

### Withheld values

When a step's output is withheld, its other captures may hold the secret too. They become
**withheld values**:

- They still work in `when`, `assert`, `until` and `fail`, which run inside korin and print
  nothing.
- They show as `«withheld:name»` wherever they would be displayed.
- korin refuses to pass them to any command: as an argument, in `env`, or through a
  template. It checks before the command starts, and stops the run with a message naming
  the value.

`korin check` warns when a plain capture from a secret-producing step reaches a command,
because at run time it may be withheld.

## What korin cannot hide

- Anything an `interactive` program prints. korin does not see the screen during a handoff.
- Files korin does not own. A secret file capture must read from `{korin.out}`, which korin
  deletes when the step ends.
- Arguments, through `ps`, while a command runs. Use `env`.

See [Keep secrets out of logs](../guides/secrets.md) for a walkthrough.
