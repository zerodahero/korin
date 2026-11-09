// External scanner for the korin grammar.
//
// It produces the tokens a context-free grammar cannot:
//
// - NEWLINE, INDENT and DEDENT. Blocks are delimited by indentation, as in
//   `src/parser.rs`: blank lines and comment-only lines do not count.
// - JQ_CHUNK: one whitespace-separated piece of a jq filter. A filter runs to the end of
//   the line, except for a trailing `on <expect>`, which the scanner leaves to the grammar.
// - JQ_VARIANT: `list` or `json` right after `jq`.

#include "tree_sitter/array.h"
#include "tree_sitter/parser.h"

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

enum TokenType {
  NEWLINE,
  INDENT,
  DEDENT,
  JQ_CHUNK,
  JQ_VARIANT,
  ERROR_SENTINEL,
};

typedef struct {
  Array(uint16_t) indents;
} Scanner;

static inline void advance(TSLexer *lexer) { lexer->advance(lexer, false); }

static inline void skip(TSLexer *lexer) { lexer->advance(lexer, true); }

static inline bool is_space(int32_t c) { return c == ' ' || c == '\t' || c == '\r'; }

static inline bool is_line_end(TSLexer *lexer) {
  return lexer->eof(lexer) || lexer->lookahead == '\n';
}

static inline bool is_ident_start(int32_t c) {
  return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c == '_';
}

static inline bool is_ident_char(int32_t c) {
  return is_ident_start(c) || (c >= '0' && c <= '9');
}

// Whether the rest of the line, from the lexer's position, is ` <identifier>` and then only
// whitespace or a comment. Consumes what it reads; the caller has already marked the end.
static bool rest_is_trailing_on_name(TSLexer *lexer) {
  if (!is_space(lexer->lookahead)) {
    return false;
  }
  while (is_space(lexer->lookahead)) {
    advance(lexer);
  }
  if (!is_ident_start(lexer->lookahead)) {
    return false;
  }
  while (is_ident_char(lexer->lookahead)) {
    advance(lexer);
  }
  if (is_line_end(lexer)) {
    return true;
  }
  if (!is_space(lexer->lookahead)) {
    return false;
  }
  while (is_space(lexer->lookahead)) {
    advance(lexer);
  }
  return is_line_end(lexer) || lexer->lookahead == '#';
}

typedef enum {
  JQ_FOUND,
  // Nothing but whitespace was read: the line ends or a comment starts here.
  JQ_NOTHING,
  // A trailing `on <expect>` was read; the grammar lexes it instead.
  JQ_REJECTED,
} JqResult;

// Scans one chunk of a jq filter.
static JqResult scan_jq(TSLexer *lexer, const bool *valid_symbols) {
  while (is_space(lexer->lookahead)) {
    skip(lexer);
  }
  if (is_line_end(lexer) || lexer->lookahead == '#') {
    return JQ_NOTHING;
  }

  // The first few characters are enough to recognise `on`, `list` and `json`.
  char head[5] = {0};
  uint32_t length = 0;
  while (!is_line_end(lexer) && !is_space(lexer->lookahead)) {
    int32_t c = lexer->lookahead;
    if (length < 5) {
      head[length] = c < 128 ? (char)c : '\x7f';
    }
    length++;
    advance(lexer);
    if (c == '"') {
      // A quoted jq string may hold spaces and `#`.
      while (!is_line_end(lexer) && lexer->lookahead != '"') {
        if (lexer->lookahead == '\\') {
          advance(lexer);
          if (is_line_end(lexer)) {
            break;
          }
        }
        advance(lexer);
        length++;
      }
      if (lexer->lookahead == '"') {
        advance(lexer);
        length++;
      }
    }
  }
  lexer->mark_end(lexer);

  if (valid_symbols[JQ_VARIANT] && length == 4 &&
      (memcmp(head, "list", 4) == 0 || memcmp(head, "json", 4) == 0)) {
    lexer->result_symbol = JQ_VARIANT;
    return JQ_FOUND;
  }
  if (!valid_symbols[JQ_CHUNK] ||
      (length == 2 && memcmp(head, "on", 2) == 0 && rest_is_trailing_on_name(lexer))) {
    return JQ_REJECTED;
  }
  lexer->result_symbol = JQ_CHUNK;
  return JQ_FOUND;
}

static bool scan_indentation(Scanner *scanner, TSLexer *lexer, const bool *valid_symbols) {
  // Skip blank lines. A comment-only line is left to the grammar as an extra, and the
  // scanner runs again after it.
  while (is_space(lexer->lookahead) || lexer->lookahead == '\n') {
    skip(lexer);
  }
  if (!lexer->eof(lexer) && lexer->lookahead == '#') {
    return false;
  }
  uint32_t column = lexer->eof(lexer) ? 0 : lexer->get_column(lexer);
  uint16_t current = *array_back(&scanner->indents);

  if (column > current && valid_symbols[INDENT]) {
    array_push(&scanner->indents, (uint16_t)column);
    lexer->result_symbol = INDENT;
    return true;
  }
  if (column < current && valid_symbols[DEDENT] && scanner->indents.size > 1) {
    array_pop(&scanner->indents);
    lexer->result_symbol = DEDENT;
    return true;
  }
  return false;
}

bool tree_sitter_korin_external_scanner_scan(void *payload, TSLexer *lexer,
                                             const bool *valid_symbols) {
  Scanner *scanner = (Scanner *)payload;

  if (valid_symbols[ERROR_SENTINEL]) {
    return false;
  }

  if (valid_symbols[JQ_CHUNK] || valid_symbols[JQ_VARIANT]) {
    JqResult result = scan_jq(lexer, valid_symbols);
    if (result != JQ_NOTHING) {
      return result == JQ_FOUND;
    }
  }

  if (valid_symbols[NEWLINE]) {
    while (is_space(lexer->lookahead)) {
      skip(lexer);
    }
    if (is_line_end(lexer)) {
      if (!lexer->eof(lexer)) {
        advance(lexer);
      }
      lexer->mark_end(lexer);
      lexer->result_symbol = NEWLINE;
      return true;
    }
    return false;
  }

  if (valid_symbols[INDENT] || valid_symbols[DEDENT]) {
    return scan_indentation(scanner, lexer, valid_symbols);
  }

  return false;
}

void *tree_sitter_korin_external_scanner_create(void) {
  Scanner *scanner = calloc(1, sizeof(Scanner));
  array_init(&scanner->indents);
  array_push(&scanner->indents, 0);
  return scanner;
}

void tree_sitter_korin_external_scanner_destroy(void *payload) {
  Scanner *scanner = (Scanner *)payload;
  array_delete(&scanner->indents);
  free(scanner);
}

unsigned tree_sitter_korin_external_scanner_serialize(void *payload, char *buffer) {
  Scanner *scanner = (Scanner *)payload;
  unsigned size = 0;
  // The base level 0 is implicit; store the rest.
  for (uint32_t i = 1; i < scanner->indents.size; i++) {
    if (size + sizeof(uint16_t) > TREE_SITTER_SERIALIZATION_BUFFER_SIZE) {
      break;
    }
    memcpy(&buffer[size], &scanner->indents.contents[i], sizeof(uint16_t));
    size += sizeof(uint16_t);
  }
  return size;
}

void tree_sitter_korin_external_scanner_deserialize(void *payload, const char *buffer,
                                                    unsigned length) {
  Scanner *scanner = (Scanner *)payload;
  array_clear(&scanner->indents);
  array_push(&scanner->indents, 0);
  for (unsigned i = 0; i + sizeof(uint16_t) <= length; i += sizeof(uint16_t)) {
    uint16_t indent;
    memcpy(&indent, &buffer[i], sizeof(uint16_t));
    array_push(&scanner->indents, indent);
  }
}
