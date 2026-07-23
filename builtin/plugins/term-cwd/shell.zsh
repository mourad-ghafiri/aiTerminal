# Report cwd + host to the terminal via OSC 7 on every prompt and `cd`, so the status
# bar reflects the live folder + user@host instantly (and the REMOTE path/host over SSH).
# Unconditional — independent of any prompt theming. zsh hooks compose, so order-safe.
autoload -Uz add-zsh-hook 2>/dev/null
__tt_osc7() { printf '\033]7;file://%s%s\033\\' "${HOST:-${HOSTNAME:-}}" "$PWD" }
add-zsh-hook -Uz chpwd  __tt_osc7 2>/dev/null
add-zsh-hook -Uz precmd __tt_osc7 2>/dev/null
__tt_osc7   # seed the initial directory before the first prompt
