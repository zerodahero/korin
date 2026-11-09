((comment) @injection.content
  (#set! injection.language "comment"))

((jq_filter) @injection.content
  (#set! injection.language "jq"))

((regex_pattern) @injection.content
  (#set! injection.language "regex"))
