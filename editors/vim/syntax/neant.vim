" Vim syntax file for neant (.nt) — a k/q-style array language.
" Mirrors src/lex.rs: // comments, "strings", `symbols, 0x bytes, 101b bools,
" 0N/0W nulls, yyyy.mm.dd dates, hh:mm:ss times, verbs +-*%!&|<>=~,^#_$?@, adverbs ' / \ /: \:
if exists("b:current_syntax") | finish | endif

syn case match

" Control forms and select clauses. \< so `if` inside `tif` is not matched.
syn keyword ntCond     if while do select by from where
syn keyword ntBuiltin  exp log sin cos tan atan rand rseed band bor bxor shl shr bnot
syn keyword ntBuiltin  key value group isnull now show print signal exit lex parse compile exec
syn keyword ntAdverbKw each over scan
syn keyword ntInfixKw  mod div xexp in within vs sv ss except inter union cross fill like

" Implicit lambda args
syn keyword ntArg x y z

" Operators. Defined before literals: at equal start the LAST rule wins, so -1 and // stay Number/Comment.
syn match ntAdverb   "[/\\]:"
syn match ntAdverb   "['/\\]"
syn match ntVerb     "[-+*%!&|<>=~,^#_$?@]"
syn match ntVerb     "::\?"
syn match ntDelim    "[][(){}]"
syn match ntSep      ";"

syn match ntComment  "//.*$" contains=@Spell
syn region ntString  start=+"+ skip=+\\.+ end=+"+ contains=ntEscape
syn match ntEscape   contained "\\[ntr0\\\"]"

" `sym or `a`b`c vector; a bare ` is the empty symbol
syn match ntSymbol   "`[A-Za-z0-9_.]*"

" Numbers: bytes, bools, dates, times, nulls/infinities, ints/floats (with optional sign)
" Generic int/float first; the specific literal shapes below win at equal start (last rule wins).
syn match ntNumber   "-\?\d\@<!\d\+\(\.\d*\)\?\([eE][+-]\?\d\+\)\?"
syn match ntNumber   "-\?\.\d\+\([eE][+-]\?\d\+\)\?"
syn match ntNumber   "\<0x\x\+\>"
syn match ntNumber   "\<[01]\+b\>"
syn match ntNumber   "\<\d\{4}\.\d\{2}\.\d\{2}\>"
syn match ntNumber   "\<\d\{1,2}:\d\{2}\(:\d\{2}\(\.\d\{1,3}\)\?\)\?\>"
syn match ntNumber   "-\?\<0[NWnw]\>"

" Assignment  name: expr   /   name:: expr (global)
syn match ntAssign   "\<[A-Za-z_][A-Za-z0-9_.]*\ze::\?"

" Lambda arg list {[a;b] ...}
syn match ntLambdaHead "{\[[^]]*\]" contains=ntDelim,ntSep,ntParam
syn match ntParam    contained "[A-Za-z_][A-Za-z0-9_]*"

hi def link ntComment   Comment
hi def link ntString    String
hi def link ntEscape    SpecialChar
hi def link ntSymbol    Constant
hi def link ntNumber    Number
hi def link ntCond      Conditional
hi def link ntBuiltin   Function
hi def link ntAdverbKw  Repeat
hi def link ntInfixKw   Operator
hi def link ntArg       Identifier
hi def link ntParam     Identifier
hi def link ntAssign    Type
hi def link ntAdverb    Special
hi def link ntVerb      Operator
hi def link ntDelim     Delimiter
hi def link ntSep       Delimiter

let b:current_syntax = "neant"
