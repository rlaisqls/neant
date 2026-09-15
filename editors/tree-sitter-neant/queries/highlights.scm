; Highlight query for neant. Standard capture names so any tree-sitter host
; (Neovim, Helix, Zed, ...) themes it without extra config.

(comment) @comment

(string) @string
(escape_sequence) @string.escape
(symbol) @string.special.symbol

(number) @number

(break_statement) @keyword.return
["if" "while" "do" "select" "by" "from" "where"] @keyword

(compound_op) @operator
(index_compound_close) @operator
(verb) @operator
(adverb) @operator.special
["::" ":" "]:"] @operator

(lambda parameters: (parameter_list (name) @variable.parameter))

(assignment name: (name) @variable)
(global_assignment name: (name) @variable)
(compound_assignment name: (name) @variable)
(index_assignment name: (name) @variable)
(compound_index_assignment name: (name) @variable)

(select_item name: (name) @property)

; `f[...]`: the value immediately before call_args, in the same flattened primary, is the callee.
(expression (name) @function.call . (call_args))

(name) @variable

["(" ")" "[" "]" "{" "}"] @punctuation.bracket
; `;`/newline separators are a regex token (_sep), not a literal one, so they aren't queryable by
; text here the way `,` is — a minor, purely cosmetic gap.
"," @punctuation.delimiter
