/**
 * @file Tree-sitter grammar for korin wizard and module files (`.korin`).
 * @license Apache-2.0
 *
 * The reference parser is `src/parser.rs`; the docs site (`docs-site/`) describes the language. This
 * grammar is for editors: it accepts everything the reference parser accepts, and is looser
 * about ordering rules (header before stages, `define` only in modules) that `korin check`
 * reports anyway.
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

const PREC = {
  or: 1,
  and: 2,
  not: 3,
  compare: 4,
  // An immediate word fragment beats a new word or a comment at the same position.
  immediate: 1,
};

const IDENTIFIER = /[A-Za-z_][A-Za-z0-9_]*/;

/**
 * Word fragments differ only in which characters end them. `start` excludes `#`, because a
 * `#` after whitespace starts a comment; a `#` inside a word does not.
 */
const FRAGMENTS = {
  // A command word ends at whitespace.
  argv: { start: /[^\s"{\\#][^\s"{\\]*/, rest: /[^\s"{\\]+/ },
  // An `allow` entry also ends at a comma.
  list: { start: /[^\s"{\\#,][^\s"{\\,]*/, rest: /[^\s"{\\,]+/ },
  // A template argument also ends at a comma or a closing parenthesis.
  arg: { start: /[^\s"{\\#,)][^\s"{\\,)]*/, rest: /[^\s"{\\,)]+/ },
};

module.exports = grammar({
  name: 'korin',

  word: $ => $.identifier,

  extras: $ => [/\s/, $.comment],

  externals: $ => [
    $._newline,
    $._indent,
    $._dedent,
    $._jq_chunk,
    $.jq_variant,
    // Never used by a rule: it is valid only during error recovery, so the scanner can
    // tell recovery apart from a real parse state.
    $._error_sentinel,
  ],

  supertypes: $ => [$._step, $._option, $._expression],

  rules: {
    source_file: $ => repeat($._top_level),

    _top_level: $ => choice(
      $.version,
      $.title,
      $.gates,
      $.mode,
      $.unattended,
      $.allow,
      $.use,
      $.param,
      $.stage,
      $.define,
    ),

    // ---- header ----

    version: $ => seq('korin', field('version', $.integer), $._newline),

    title: $ => seq('title', field('text', $.string), $._newline),

    gates: $ => seq('gates', field('mode', choice('all', 'mutating')), $._newline),

    mode: $ => seq('mode', field('mode', choice('stepwise', 'guide')), $._newline),

    unattended: $ => seq('unattended', 'allowed', $._newline),

    allow: $ => seq(
      'allow',
      commaSep1(field('executable', alias($._list_word, $.word))),
      $._newline,
    ),

    use: $ => seq(
      'use',
      field('module', choice($.identifier, $.string)),
      optional(seq('as', field('alias', $.identifier))),
      $._newline,
    ),

    param: $ => seq(
      'param',
      field('name', $.identifier),
      choice('required', 'secret', seq('=', field('default', $.string))),
      $._newline,
    ),

    // ---- blocks ----

    stage: $ => seq('stage', field('title', $.string), $._newline, field('body', $.block)),

    define: $ => seq(
      'define',
      field('name', $.identifier),
      '(',
      optional(commaSep1(field('parameter', $.identifier))),
      ')',
      $._newline,
      field('body', $.define_body),
    ),

    define_body: $ => seq($._indent, optional($.exports), repeat1($._step), $._dedent),

    exports: $ => seq('exports', commaSep1(field('name', $.identifier)), $._newline),

    block: $ => seq($._indent, repeat1($._step), $._dedent),

    // ---- steps ----

    _step: $ => choice(
      $.command,
      $.confirm,
      $.ask,
      $.assert,
      $.when,
      $.each,
      $.poll,
      $.say,
      $.call,
    ),

    command: $ => seq(
      choice('read', 'mutate', 'interactive', seq('script', choice('read', 'mutate'))),
      field('executable', $.word),
      repeat(field('argument', $.word)),
      $._newline,
      optional(field('options', $.options)),
    ),

    options: $ => seq($._indent, repeat1($._option), $._dedent),

    confirm: $ => seq('confirm', field('text', $.string), $._newline),

    ask: $ => seq(
      'ask',
      optional('secret'),
      field('name', $.identifier),
      field('prompt', $.string),
      $._newline,
    ),

    assert: $ => seq(
      'assert',
      field('condition', $._expression),
      'else',
      field('message', $.string),
      $._newline,
    ),

    when: $ => seq(
      'when',
      field('condition', $._expression),
      $._newline,
      field('body', $.block),
    ),

    each: $ => seq(
      'each',
      field('variable', $.identifier),
      'in',
      field('list', $._expression),
      $._newline,
      field('body', $.block),
    ),

    poll: $ => seq(
      'poll',
      'every',
      field('every', $.duration),
      'for',
      field('limit', $.duration),
      $._newline,
      field('body', $.poll_body),
    ),

    poll_body: $ => seq($._indent, repeat1(choice($._step, $.until, $.fail)), $._dedent),

    until: $ => seq('until', field('condition', $._expression), $._newline),

    fail: $ => seq('fail', field('condition', $._expression), $._newline),

    say: $ => seq(
      field('kind', choice('note', 'warn', 'record')),
      field('text', $.string),
      $._newline,
    ),

    call: $ => seq(
      field('module', $.identifier),
      token.immediate('.'),
      field('template', $.identifier),
      '(',
      optional(commaSep1(field('argument', alias($._arg_word, $.word)))),
      ')',
      optional(seq('as', field('alias', $.identifier))),
      $._newline,
    ),

    // ---- command options ----

    _option: $ => choice($.capture, $.expect, $.env, $.timeout),

    capture: $ => seq(
      'capture',
      optional('secret'),
      field('name', $.identifier),
      optional(field('optional', alias(token.immediate('?'), '?'))),
      '=',
      optional(seq('file', field('file', $.word))),
      field('mode', $.capture_mode),
      optional(seq('on', field('on', $.identifier))),
      $._newline,
    ),

    capture_mode: $ => choice(
      seq('jq', optional($.jq_variant), field('filter', $.jq_filter)),
      'text',
      'lines',
      seq('regex', field('pattern', $.regex)),
    ),

    // The scanner hands out the filter one whitespace-separated chunk at a time, so it can
    // leave a trailing `on <expect>` to the grammar.
    jq_filter: $ => repeat1($._jq_chunk),

    expect: $ => seq(
      'expect',
      field('name', $.identifier),
      '=',
      sep1(field('match', $.match), 'and'),
      $._newline,
    ),

    match: $ => choice(
      seq('exit', choice(field('code', $.integer), seq('!=', field('code', $.integer)))),
      seq('signal', field('signal', $.identifier)),
      seq(choice('stdout', 'stderr'), '~', field('pattern', $.regex)),
      seq('missing', field('path', $.word)),
    ),

    env: $ => seq(
      'env',
      field('name', $.identifier),
      token.immediate('='),
      field('value', $.word),
      $._newline,
    ),

    timeout: $ => seq('timeout', field('duration', $.duration), $._newline),

    // ---- expressions ----

    _expression: $ => choice(
      $.or,
      $.and,
      $.not,
      $.comparison,
      $.membership,
      $.regex_match,
      $._primary,
    ),

    or: $ => prec.left(PREC.or, seq(
      field('left', $._expression), 'or', field('right', $._expression),
    )),

    and: $ => prec.left(PREC.and, seq(
      field('left', $._expression), 'and', field('right', $._expression),
    )),

    not: $ => prec(PREC.not, seq('not', field('operand', $._expression))),

    comparison: $ => prec(PREC.compare, seq(
      field('left', $._primary),
      field('operator', choice('==', '!=', '<', '<=', '>', '>=')),
      field('right', $._primary),
    )),

    membership: $ => prec(PREC.compare, seq(
      field('left', $._primary), 'in', field('right', $._primary),
    )),

    regex_match: $ => prec(PREC.compare, seq(
      field('left', $._primary), '~', field('pattern', $.regex),
    )),

    _primary: $ => choice(
      $.reference,
      $.string,
      $.number,
      $.list,
      $.exists,
      $.parenthesized,
    ),

    exists: $ => seq('exists', field('name', $.reference)),

    reference: $ => seq($.identifier, repeat(seq(token.immediate('.'), $.identifier))),

    list: $ => seq('[', optional(commaSep1($._primary)), ']'),

    parenthesized: $ => seq('(', $._expression, ')'),

    // ---- words and strings ----

    word: $ => makeWord($, '_argv_fragment'),
    _list_word: $ => makeWord($, '_list_fragment'),
    _arg_word: $ => makeWord($, '_arg_fragment'),

    _argv_fragment: _ => FRAGMENTS.argv.start,
    _argv_fragment_rest: _ => token.immediate(prec(PREC.immediate, FRAGMENTS.argv.rest)),
    _list_fragment: _ => FRAGMENTS.list.start,
    _list_fragment_rest: _ => token.immediate(prec(PREC.immediate, FRAGMENTS.list.rest)),
    _arg_fragment: _ => FRAGMENTS.arg.start,
    _arg_fragment_rest: _ => token.immediate(prec(PREC.immediate, FRAGMENTS.arg.rest)),

    string: $ => seq('"', $._string_body),

    // A quoted segment in the middle of a word, as in `--name="two words"`.
    _string_continued: $ => seq(token.immediate(prec(PREC.immediate, '"')), $._string_body),

    _string_body: $ => seq(
      repeat(choice(
        $._string_content,
        alias($._escape_continued, $.escape_sequence),
        alias($._interpolation_continued, $.interpolation),
      )),
      token.immediate('"'),
    ),

    _string_content: _ => token.immediate(prec(PREC.immediate, /[^"\\{\n]+/)),

    escape_sequence: _ => /\\./,
    _escape_continued: _ => token.immediate(prec(PREC.immediate, /\\./)),

    interpolation: $ => seq('{', $._interpolation_body),
    _interpolation_continued: $ => seq(
      token.immediate(prec(PREC.immediate, '{')),
      $._interpolation_body,
    ),

    _interpolation_body: $ => seq(
      alias($._immediate_identifier, $.identifier),
      repeat(seq(token.immediate('.'), alias($._immediate_identifier, $.identifier))),
      optional(alias(token.immediate('...'), '...')),
      token.immediate('}'),
    ),

    _immediate_identifier: _ => token.immediate(IDENTIFIER),

    // ---- tokens ----

    // A regex runs to the next unescaped `/`, as in `src/parser.rs`.
    regex: $ => seq('/', optional(field('pattern', $.regex_pattern)), token.immediate('/')),

    regex_pattern: _ => token.immediate(/(\\.|[^\/\\\n])+/),

    identifier: _ => IDENTIFIER,

    integer: _ => /[0-9]+/,

    number: _ => /-?[0-9][0-9.]*/,

    duration: _ => /[0-9]+[smh]/,

    comment: _ => token(seq('#', /.*/)),
  },
});

/**
 * One command word: literal text, `"quoted"` segments, `\` escapes and `{name}`
 * interpolations, with no whitespace between them.
 *
 * @param {GrammarSymbols<string>} $
 * @param {string} fragment the name of the rule for the literal text that starts a word
 * @returns {SeqRule}
 */
function makeWord($, fragment) {
  return seq(
    choice($[fragment], $.string, $.escape_sequence, $.interpolation),
    repeat(choice(
      $[`${fragment}_rest`],
      alias($._string_continued, $.string),
      alias($._escape_continued, $.escape_sequence),
      alias($._interpolation_continued, $.interpolation),
    )),
  );
}

/**
 * @param {RuleOrLiteral} rule
 * @returns {SeqRule}
 */
function commaSep1(rule) {
  return sep1(rule, ',');
}

/**
 * @param {RuleOrLiteral} rule
 * @param {RuleOrLiteral} separator
 * @returns {SeqRule}
 */
function sep1(rule, separator) {
  return seq(rule, repeat(seq(separator, rule)));
}
