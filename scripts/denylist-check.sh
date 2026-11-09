#!/usr/bin/env bash
# Fails when a file contains a pattern from a private denylist.
#
# The denylist lives outside the repo, so the patterns themselves are never
# committed: one extended regex per line, blank lines and `#` comments ignored.
#
# Usage: scripts/denylist-check.sh FILE...
set -euo pipefail

list="${KORIN_DENYLIST:-$HOME/.config/korin/denylist}"

if [[ ! -f "$list" ]]; then
    printf 'denylist-check: %s not found; skipping. See CONTRIBUTING.md.\n' "$list" >&2
    exit 0
fi

patterns=$(mktemp)
trap 'rm -f "$patterns"' EXIT
grep -vE '^[[:space:]]*(#|$)' "$list" >"$patterns" || true

if [[ ! -s "$patterns" ]]; then
    exit 0
fi

(($# > 0)) || exit 0

# -I skips binary files. Print only file:line, never the match, so a hit never
# echoes a private pattern into CI logs. grep exits 0 on a match, 1 on none and 2
# on an error (a bad pattern, an unreadable file): an error must fail the check,
# never pass it.
status=0
matches=$(grep -H -I -n -i -E -f "$patterns" -- "$@") || status=$?
case "$status" in
0)
    printf 'denylist-check: private names found at:\n%s\n' "$(printf '%s\n' "$matches" | cut -d: -f1,2)" >&2
    exit 1
    ;;
1) exit 0 ;;
*)
    printf 'denylist-check: grep failed (exit %s); check the patterns in %s and the files given\n' "$status" "$list" >&2
    exit 2
    ;;
esac
