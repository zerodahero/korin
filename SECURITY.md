# Security policy

## Reporting a vulnerability

Please report security issues privately through
[GitHub security advisories](https://github.com/zerodahero/korin/security/advisories/new).
Do not open a public issue.

Include the korin version, a minimal `.korin` file that shows the problem, and what you
expected to happen.

## Scope

korin's core promises are security properties, so a break in any of them is in scope:

- A command runs without being shown first, or without passing its gate.
- A secret value reaches the terminal, the journal, the report or an output file.
- A withheld value reaches an external command.
- A `{value}` interpolation changes the number of arguments a command receives.

Output that an `interactive` child writes to the terminal is outside korin's control. So is
argv that other local users can read through `ps`. See `docs/language.md`.
