"""A Pygments lexer for korin wizard and module files (`.korin`).

The reference grammar is korin's `src/parser.rs`; the tree-sitter grammar in
`tree-sitter-korin/` is the full one for editors. This lexer only colors the docs site, so it
works line by line and never needs to know which block a line is in. There is no catch-all
rule: text it does not expect becomes an Error token, and `check_lexer` fails on those.
"""

from pygments.lexer import RegexLexer, bygroups, include
from pygments.token import (
    Comment,
    Keyword,
    Name,
    Number,
    Operator,
    Punctuation,
    String,
    Text,
    Whitespace,
)

# Words that start a line, or appear inside one, with a fixed meaning.
KEYWORDS = (
    "korin title gates unattended allowed allow use as param required secret stage define "
    "exports confirm ask assert else when each in poll every for until fail note warn record "
    "capture expect env timeout and or not exists exit signal stdout stderr missing on file "
    "all mutating text lines list json"
).split()

IDENT = r"[A-Za-z_][A-Za-z0-9_]*"


class KorinLexer(RegexLexer):
    name = "korin"
    aliases = ["korin"]
    filenames = ["*.korin"]
    mimetypes = ["text/x-korin"]

    tokens = {
        "comment": [
            # A comment starts at `#` at the start of a line or after a space.
            (r"(^|(?<=\s))#.*?$", Comment.Single),
        ],
        "reference": [
            (r"\{[A-Za-z_][A-Za-z0-9_.$]*(\.\.\.)?\}", Name.Variable),
        ],
        "string": [
            (r'"', String.Double, "string-body"),
        ],
        "string-body": [
            (r'\\["\\{}#,) nt]', String.Escape),
            include("reference"),
            (r'[^"\\{]+', String.Double),
            (r"\{", String.Double),
            (r'"', String.Double, "#pop"),
        ],
        "regex": [
            (r"/(\\.|[^/\\\n])*/", String.Regex),
        ],
        "root": [
            (r"\n", Whitespace),
            (r"[ ]+", Whitespace),
            include("comment"),
            # Commands: the rest of the line is argv.
            (
                r"(script)([ ]+)(read|mutate)\b",
                bygroups(Keyword, Whitespace, Keyword),
                "executable",
            ),
            (r"(read|mutate|interactive)\b(?=[ ])", Keyword, "executable"),
            # Capture modes take their own syntax to the end of the line.
            (
                r"(jq)([ ]+)(?:(list|json)([ ]+))?",
                bygroups(Keyword, Whitespace, Keyword, Whitespace),
                "jq",
            ),
            (r"(regex)([ ]+)", bygroups(Keyword, Whitespace)),
            (r"(env)([ ]+)([A-Za-z_][A-Za-z0-9_]*)(=)", bygroups(Keyword, Whitespace, Name.Constant, Operator), "argv"),
            # `file <path>` in a capture takes one word as its path.
            (r"(file)([ ]+)", bygroups(Keyword, Whitespace), "word"),
            (r"(missing)([ ]+)(?!=)", bygroups(Keyword, Whitespace), "word"),
            # A declared name is a name even when it spells a keyword: `expect missing = …`.
            (
                r"(expect|param|each)([ ]+)(" + IDENT + r")\b",
                bygroups(Keyword, Whitespace, Name.Variable),
            ),
            (
                r"(capture|ask)((?:[ ]+secret)?)([ ]+)(" + IDENT + r"\??)(?=[ ]|$)",
                bygroups(Keyword, Keyword, Whitespace, Name.Variable),
            ),
            # A template call: `module.template(args)`.
            (
                r"(" + IDENT + r")(\.)(" + IDENT + r")(\()",
                bygroups(Name.Namespace, Punctuation, Name.Function, Punctuation),
                "call-args",
            ),
            (r"(define)([ ]+)(" + IDENT + r")", bygroups(Keyword, Whitespace, Name.Function)),
            (r"(korin)([ ]+)(\d+)", bygroups(Keyword, Whitespace, Number.Integer)),
            (r"\b(" + "|".join(KEYWORDS) + r")\b", Keyword),
            include("string"),
            include("regex"),
            include("reference"),
            (r"\d+[smh]\b", Number),
            (r"-?\d+(\.\d+)?\b", Number),
            (r"(" + IDENT + r")(\.)(" + IDENT + r")", bygroups(Name.Namespace, Punctuation, Name.Variable)),
            (IDENT + r"\??", Name.Variable),
            (r"==|!=|<=|>=|<|>|~|=", Operator),
            (r"[()\[\],]", Punctuation),
        ],
        # The first word of a command is the executable.
        "executable": [
            (r"[ ]+", Whitespace),
            (r'[^ \n{"#]+', Name.Builtin, ("#pop", "argv")),
            (r"\n", Whitespace, "#pop"),
        ],
        "argv": [
            (r"\n", Whitespace, "#pop"),
            (r"[ ]+", Whitespace),
            include("comment"),
            include("string"),
            (r'\\["\\{}#,) nt]', String.Escape),
            include("reference"),
            (r"--?[A-Za-z][A-Za-z0-9-]*", Name.Attribute),
            (r"\{", Text),
            (r'[^ \n{"\\]+', Text),
        ],
        "word": [
            include("string"),
            include("reference"),
            (r'[^ \n{"]+', Text),
            (r"(?=[ \n])", Text, "#pop"),
        ],
        # A jq filter runs to the end of the line, except a trailing `on <expect>`.
        "jq": [
            (r"(?=\n)", Text, "#pop"),
            (r"([ ]+)(on)([ ]+)(" + IDENT + r")(?=[ ]*(#.*)?\n)", bygroups(Whitespace, Keyword, Whitespace, Name.Variable)),
            (r"[ ]+#.*?(?=\n)", Comment.Single),
            (r'"(\\.|[^"\\\n])*"', String.Double),
            (r"\.[A-Za-z_][A-Za-z0-9_]*", Name.Tag),
            (r"\b(select|map|length|keys|values|has|test|not|and|or|if|then|else|end|empty|first|last)\b", Keyword.Pseudo),
            (r"[|\[\]().,:?=<>!+*/-]+", Operator),
            (r"\d+", Number),
            (r"[ ]+", Whitespace),
            (r"[A-Za-z_$][A-Za-z0-9_]*", Name),
        ],
        "call-args": [
            (r"\)", Punctuation, "#pop"),
            (r",", Punctuation),
            (r"[ ]+", Whitespace),
            include("string"),
            include("reference"),
            (r'[^ \n{",)]+', Text),
        ],
    }
