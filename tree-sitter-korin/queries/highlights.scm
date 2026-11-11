; Capture names follow the nvim-treesitter conventions. Helix falls back to the parent
; scope for any name its theme lacks, such as `keyword` for `keyword.import`.

(comment) @comment @spell

; Keywords

[
  "korin"
  "title"
  "gates"
  "mode"
  "unattended"
  "allow"
  "param"
  "stage"
  "exports"
  "confirm"
  "ask"
  "note"
  "warn"
  "record"
  "capture"
  "expect"
  "env"
  "timeout"
  "file"
  "on"
  "exit"
  "signal"
  "stdout"
  "stderr"
  "missing"
  "jq"
  "text"
  "lines"
  "regex"
  "as"
] @keyword

(jq_variant) @keyword

"define" @keyword.function

"use" @keyword.import

[
  "when"
  "assert"
  "else"
] @keyword.conditional

[
  "each"
  "poll"
  "every"
  "for"
  "until"
  "fail"
] @keyword.repeat

[
  "and"
  "or"
  "not"
  "in"
  "exists"
] @keyword.operator

[
  "read"
  "mutate"
  "script"
  "interactive"
] @keyword

[
  "all"
  "mutating"
  "stepwise"
  "guide"
  "allowed"
  "required"
  "secret"
] @keyword.modifier

; Names

(param
  name: (identifier) @variable.parameter)

(define
  name: (identifier) @function)

(define
  parameter: (identifier) @variable.parameter)

(exports
  name: (identifier) @variable)

(use
  module: (identifier) @module)

(use
  alias: (identifier) @module)

(call
  module: (identifier) @module)

(call
  template: (identifier) @function.call)

(call
  alias: (identifier) @variable)

(capture
  name: (identifier) @variable)

(capture
  on: (identifier) @label)

(expect
  name: (identifier) @label)

(ask
  name: (identifier) @variable)

(each
  variable: (identifier) @variable)

(env
  name: (identifier) @constant)

(match
  signal: (identifier) @constant)

(reference
  (identifier) @variable)

(interpolation
  (identifier) @variable)

; Commands

(command
  executable: (word) @function.call)

(allow
  executable: (word) @function.call)

(command
  argument: (word) @variable.parameter
  (#match? @variable.parameter "^-"))

; Literals

(string) @string

(escape_sequence) @string.escape

(regex) @string.regexp

(jq_filter) @string.special

[
  (integer)
  (number)
] @number

(duration) @number

; Punctuation and operators

(interpolation
  [
    "{"
    "}"
  ] @punctuation.special)

(interpolation
  "..." @punctuation.special)

[
  "("
  ")"
  "["
  "]"
] @punctuation.bracket

[
  ","
  "."
] @punctuation.delimiter

[
  "="
  "=="
  "!="
  "<"
  "<="
  ">"
  ">="
  "~"
] @operator

(capture
  optional: "?" @operator)
