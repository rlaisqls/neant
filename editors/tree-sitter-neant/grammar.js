// tree-sitter grammar for neant (.nt), mirroring src/lex.rs and src/parse.rs.
//
// ponytail: neant's real lexer decides `-2` vs `- 2` from the PREVIOUS TOKEN (a noun blocks the
// glue, see lex.rs `prev_noun`) — a context tree-sitter's regex lexer can't see without an external
// C scanner. This grammar always glues `-<digit>` into one negative-number token, so `x-2` highlights
// as `x` `-2` (number) instead of `x` `-` `2` (subtraction). Cosmetic only; upgrade path is an
// external scanner (src/scanner.c) replaying lex.rs's prev_noun check if exact tokens ever matter.
//
// Similarly this grammar does not encode neant's right-to-left, precedence-free application order
// (parse.rs `resolve`) — an `expression` is a flat run of primaries. Good enough for highlighting,
// folding and textobjects; not a semantic parser.

// ponytail: tree-sitter's regex engine has no look-around, so the `\b`-style boundary checks
// lex.rs does after 101b / 0N / 0x.. (reject a trailing alnum) are skipped here — `101bar` would
// lex as bool(101b)+name(ar) instead of one name. Rare in practice; a scanner.c fixes it if needed.
const NUMBER = token(choice(
  /0x[0-9a-fA-F]+/,                                       // byte(s): 0x0aff
  /[01]+b/,                                                // bool(s): 101b
  /\d{4}\.\d{2}\.\d{2}( \d{4}\.\d{2}\.\d{2})*/,            // date(s): 2026.09.15
  /\d{1,2}:\d{2}(:\d{2}(\.\d{1,3})?)?/,                    // time: 12:30:00.5
  /-?0[NWnw]/,                                             // null/infinity: 0N 0W 0n 0w
  /-?(\d+(\.\d*)?|\.\d+)([eE][+-]?\d+)?( -?(\d+(\.\d*)?|\.\d+)([eE][+-]?\d+)?)*/, // int/float vector
));

const VERB_CHARS = '-+*%!&|<>=~,^#_$?@';

module.exports = grammar({
  name: 'neant',

  extras: $ => [/[ \t\r]/, $.comment],

  word: $ => $.name,

  // `x[...` is ambiguous until the closing bracket (and what follows it) is reached: it could be
  // an ordinary call continuing `expression`, or the start of `index_assignment`/
  // `compound_index_assignment`. This has to be a real declared conflict (GLR forks the shared
  // `x [ args` prefix and prunes once the closing token — plain `]` vs the glued `]:`/`][verb]:` —
  // settles it) rather than resolved with static `prec()`: a static resolution picks one branch for
  // every occurrence of `name [`, even `x[y]` with no `:` at all, which then fails outright instead
  // of falling back to an ordinary call.
  conflicts: $ => [
    [$._postfixable, $.index_assignment, $.compound_index_assignment],
  ],

  rules: {
    source_file: $ => repeat(choice($._sep, $._statement)),

    _sep: _$ => /[;\n]/,

    comment: _$ => token(seq('//', /[^\n]*/)),

    _statement: $ => choice(
      $.return_statement,
      $.global_assignment,
      $.compound_index_assignment,
      $.index_assignment,
      $.compound_assignment,
      $.assignment,
      $.if_statement,
      $.while_statement,
      $.do_statement,
      $.select_statement,
      $.break_statement,
      $.expression,
    ),

    return_statement: $ => seq(':', field('value', $.expression)),
    break_statement: _$ => 'break',

    global_assignment: $ => prec(1, seq(field('name', $.name), '::', field('value', $.expression))),
    assignment: $ => prec(1, seq(field('name', $.name), ':', field('value', $.expression))),
    // `x+: e` == `x: x+e`. `op` is one glued token (verb immediately followed by `:`, no space) so
    // it never shares a grammar prefix with a plain verb primary inside an ordinary expression —
    // that shared-prefix shape is what caused `a + b` to mis-parse before this token existed.
    compound_assignment: $ => prec(1, seq(field('name', $.name), field('op', $.compound_op), field('value', $.expression))),
    // Same glued-token trick for `x[i]: e` / `x[i]+: e`: call_args' closing `]` is fused with the
    // `:` (and, for the compound form, the verb) that follows it, so index/compound_index_assignment
    // never share the plain `]` reduction that an ordinary `x[i]` call already uses — that sharing is
    // what made bare `x[y]` calls mis-parse before this split.
    index_assignment: $ => seq(field('name', $.name), '[', optional(seq(optional($.expression), repeat(seq(/[;\n]/, optional($.expression))))), ']:', field('value', $.expression)),
    compound_index_assignment: $ => seq(field('name', $.name), '[', optional(seq(optional($.expression), repeat(seq(/[;\n]/, optional($.expression))))), field('op', $.index_compound_close), field('value', $.expression)),

    if_statement: $ => seq('if', field('body', $.bracketed_statements)),
    while_statement: $ => seq('while', field('body', $.bracketed_statements)),
    do_statement: $ => seq('do', field('body', $.bracketed_statements)),

    // if[cond; ...body] / while[cond; ...body] / do[n; ...body] — condition is just the first statement.
    bracketed_statements: $ => seq('[', optional($._stmt_seq), ']'),
    _stmt_seq: $ => repeat1(choice($._sep, $._statement)),

    select_statement: $ => seq(
      'select',
      optional(field('columns', $.select_list)),
      optional(seq('by', field('by', $.select_list))),
      'from', field('from', $.expression),
      optional(seq('where', field('where', $.select_list))),
    ),
    select_list: $ => seq($.select_item, repeat(seq(',', $.select_item))),
    select_item: $ => seq(optional(prec(1, seq(field('name', $.name), ':'))), field('value', $.expression)),

    expression: $ => prec.right(repeat1($._primary)),

    // $[cond;then;cond;then;...;else] parses as an ordinary call (function = verb `$`) —
    // no dedicated cond_expression rule, which also sidesteps a lexer conflict between a bare
    // `$` verb and a literal `$` used to open a cond form.
    _postfixable: $ => choice(
      $.number, $.string, $.symbol, $.name, $.verb,
      $.paren_expression, $.lambda,
    ),

    // A primary is a value followed by zero or more postfix ops (`f[a;b]`, `f'`, `f[a]'[b]`, ...),
    // flattened as siblings rather than nested `call(function, arguments)` nodes: deliberately
    // base+repeat(suffix), not left-recursive node types referencing each other through a shared
    // base — that shape left tree-sitter's table always reducing the base early and never shifting
    // into the postfix, so plain `x[y]` never parsed as a call at all.
    _primary: $ => prec.right(seq($._postfixable, repeat($._postfix))),
    _postfix: $ => choice($.call_args, $.adverb),

    // f[a;b]  f[;2] (empty slot -> projection)  — separator is `;` or newline, same as lex.rs.
    call_args: $ => seq('[', optional(seq(optional($._arg), repeat(seq(/[;\n]/, optional($._arg))))), ']'),
    // a cond form ($[cond;val;...]) is just a call, and its "value" slots sometimes hold a full
    // statement (`node: advb node`, `:node`) rather than a bare expression — so a call_args item is
    // any statement, same as a lambda body's.
    _arg: $ => $._statement,

    paren_expression: $ => seq('(', optional($._stmt_seq), ')'),


    lambda: $ => seq('{', optional(field('parameters', $.parameter_list)), optional(field('body', $._stmt_seq)), '}'),
    parameter_list: $ => seq('[', optional(seq($.name, repeat(seq(/[;\n]/, $.name)))), ']'),

    name: _$ => /\.?[A-Za-z][A-Za-z0-9_.]*/,
    verb: _$ => token(choice(...VERB_CHARS.split(''))),
    compound_op: _$ => token(seq(choice(...VERB_CHARS.split('')), ':')),
    index_compound_close: _$ => token(seq(']', choice(...VERB_CHARS.split('')), ':')),
    adverb: _$ => token(choice("'", '/', '\\', '/:', '\\:')),

    number: _$ => NUMBER,
    string: $ => seq('"', repeat(choice($.escape_sequence, /[^"\\]/)), '"'),
    escape_sequence: _$ => /\\./,
    symbol: _$ => token(repeat1(seq('`', /[A-Za-z0-9_.]*/))),
  },
});
