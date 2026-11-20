; Planck's additions, loaded before the upstream query (first match wins).

; #define values: MAX_LEN 256, PATH "/tmp"
((preproc_arg) @constant.numeric
  (#match? @constant.numeric "^\\s*-?(0[xX][0-9a-fA-F]+|[0-9][0-9.]*)[uUlLfF]*\\s*$"))
((preproc_arg) @string
  (#match? @string "^\\s*\".*\"\\s*$"))
