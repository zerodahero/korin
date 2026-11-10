"""Fails if the korin lexer yields an Error token for any `.korin` file in the repo.

Run from `docs-site/` with `uv run python -m korin_docs.check_lexer`.
"""

import subprocess
import sys
from pathlib import Path

from pygments.token import Error

from korin_docs.lexer import KorinLexer

ROOT = Path(__file__).resolve().parents[2]
# The tree-sitter corpus holds deliberately broken files.
SKIP = "tree-sitter-korin/test/"


def main() -> int:
    lexer = KorinLexer()
    listed = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "*.korin"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.split()
    files = [ROOT / name for name in sorted(set(listed)) if not name.startswith(SKIP)]
    failures = 0
    for path in files:
        text = path.read_text()
        line = 1
        for token, value in lexer.get_tokens(text):
            if token in Error:
                print(f"{path.relative_to(ROOT)}:{line}: lexer error at {value!r}")
                failures += 1
            line += value.count("\n")
    print(f"checked {len(files)} files, {failures} lexer errors")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
