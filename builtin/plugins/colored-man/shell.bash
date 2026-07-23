# Theme the pager `less` (used by man) via LESS_TERMCAP truecolor escapes built from the
# engine-exported TT_*_RGB theme colors. $'\e' ANSI-C quoting works in bash and zsh alike.
export LESS_TERMCAP_md=$'\e[1;38;2;'"${TT_ACCENT_RGB}"$'m'     # bold → section titles
export LESS_TERMCAP_mb=$'\e[38;2;'"${TT_ERROR_RGB}"$'m'        # blink
export LESS_TERMCAP_us=$'\e[4;38;2;'"${TT_SUCCESS_RGB}"$'m'    # underline → options
export LESS_TERMCAP_so=$'\e[7;38;2;'"${TT_ACCENT2_RGB}"$'m'    # standout → status line / search
export LESS_TERMCAP_me=$'\e[0m'
export LESS_TERMCAP_ue=$'\e[0m'
export LESS_TERMCAP_se=$'\e[0m'
export GROFF_NO_SGR=1   # make grotty emit the legacy codes `less` styles
