# Write wizards with an agent

A coding agent can turn a runbook, a script or a description of a task into a wizard. korin
then holds the line between writing and running: the agent writes the file and proves it
valid, and a person reads each stage and approves it.

## Install the skill

The korin skill teaches an agent how to write a good wizard: guard every change, name the
failures that are answers, pass secrets through `env`, check the file, and hand it over
without running it. It is an [Agent Skill](https://agentskills.io): a `SKILL.md` and its
files, kept in this repository under
[`skills/korin/`](https://github.com/zerodahero/korin/tree/main/skills/korin), so it changes
with the language.

=== "Claude Code plugin"

    ```text
    /plugin install korin --marketplace zerodahero/korin
    ```

    Claude loads the skill when a task fits it. You can also call it by name: `/korin:korin`.

=== "Copy the folder"

    Copy `skills/korin/` into your agent's skills directory. For Claude Code, that is
    `~/.claude/skills/korin/` for every project, or `.claude/skills/korin/` in one
    repository. Other agents that read `SKILL.md` files work the same way.

Then ask for what you want:

> Turn `ops/rotate-certs.md` into a korin wizard. Guard each step so it is safe to run again.

## What the agent does

The skill gives the agent four steps:

1. **Scope.** List every command, mark it as a read or a change, and find the values that
   flow between commands and the failures that mean "not done yet".
2. **Write.** Follow korin's conventions: a check before every change, specific expects,
   secrets through `env`, an `allow` list, a `confirm` before the step that can't be
   undone.
3. **Check.** Run `korin check --format json` until it prints `[]`, then read `korin plan`
   as you will.
4. **Hand off.** Give you the file and the exact `korin run` command, and say which stages
   change something.

The agent does not run the wizard. Without a terminal, korin refuses to run anything unless
the wizard declares `unattended allowed` and the command passes `--yes`, so a wizard written
for a person can't be run around them.

## The check loop

`korin check --format json` reports every problem as JSON: file, line, column, a code, a
message and, usually, the fix.

```json
--8<-- "docs-site/docs/generated/check-json.txt"
```

An agent fixes each one and runs the check again until the output is `[]`. Every check runs
without running a command, so the loop is fast and safe. See
[Command line](../reference/cli.md#korin-check).

## Review what it wrote

Read the wizard the way you will run it:

- **`korin plan`** prints every stage and step, with each command's real arguments and a
  placeholder, such as `<snapshot ← 2.1>`, for each value found at run time. It marks which
  stages change something.
- **`allow`** in the header lists every program the wizard can start. `korin check` rejects
  any other, including inside templates.
- **Each `mutate`** should sit under a `when` that checks whether its change is already made.
  If one does not, ask why.

Then run it. You still approve each stage at its gate, with every value it found shown.

## Docs for agents

The site publishes [`llms.txt`](https://zerodahero.github.io/korin/llms.txt), an index of every page, and
[`llms-full.txt`](https://zerodahero.github.io/korin/llms-full.txt), every page as Markdown in one file. Point an agent at
either when it needs more than the skill carries.
