; Planck's additions, loaded before the upstream lapce/tree-sitter-grammars
; query: the first pattern that matches a node decides its color.

; ALL_CAPS names are constants by convention: NAME=..., $NAME, ${NAME}
((variable_name) @constant
  (#match? @constant "^[A-Z][A-Z0-9_]*$"))

; Unquoted assignment values: COUNT=3, NAME=foo, DIR=/usr/$NAME, LIST=(a b)
(variable_assignment value: (number) @constant.numeric.integer)
(variable_assignment value: (word) @string)
(variable_assignment value: (concatenation (word) @string))
(variable_assignment value: (array (word) @string))
