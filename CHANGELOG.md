# Changelog

Notable changes to korin, by release. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [0.2.0] - 2026-11-24

### Features

- Show the length of hidden input
- Read a user config file
- Pause on read-only stages under gates mutating
- Show a legend for path prefixes
- Add the values view
- Add the preflight block
- Skip stages, with skip rules, --skip, --set and a prompting --from
- Show scripts at the gate, open them in a pager, and pin them
- Write the run log as a shell session
- Replace the ghost and footer with a plan pane
- Show gates, confirms and pauses in the plan pane
- Run dialogs in the plan pane and leave one record each
- Show a running command in a live window and fold long output
- Open folded outputs from the o picker
- Show poll tries as one line each, with the poll's state in the pane
- End the run with a summary, and give the pane back after interactive
- Group the run log under stage banners
- Lay out the words after -- as a nested command
- Show pane steps without their kind, and mark mutating ones
- Set say lines apart with a bar and italic accent text
- Tag each command with how it ended
- Draw a box around the live prompt
- Open each item in the o picker with its own program
- Let a stage opt in to retry after a failure
- Filter values (#3)

### Fixes

- Drop the intro's legend line
- Drop the exit 0 tag, and let Esc leave a number prompt at once

### Documentation

- Describe the run log, the plan pane, folds and poll tries
- Describe retry, output, openers, icons and the new run log marks
- Put a blank line between CONTRIBUTING's list items
## [0.1.2] - 2026-11-11

### Features

- Break long positional arguments onto their own lines
- Color the executable, quoted arguments and path prefixes
- Show a compact view of paths and huge values in the terminal
- Add mode guide, a read-only walkthrough with no gates

### Documentation

- Document mode guide and the compact view
## [0.1.1] - 2026-11-11

### Features

- Make stages stand out and long commands readable
- Answer gates with a single key
## [0.1.0] - 2026-11-10

### Features

- Korin v1 (#1)
- Carbonfox colors and a live, prompt-style run view

### Fixes

- Explain that on takes an expect name
- Keep the journal's event kind on command_start

### Documentation

- Note about name
- Add conform.nvim setup
- Bump for v0.1

### CI

- Mirror the tree-sitter grammar to its own repo

