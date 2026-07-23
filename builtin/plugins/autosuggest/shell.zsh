# History-based inline autosuggestion: the most recent matching history entry is shown DIMMED
# (theme $TT_MUTED) after the cursor; → (or End) accepts it and it becomes a normal command.
#
# We own our own dim and never depend on another plugin for it: reset region_highlight at the top
# of every redraw (so an accepted suggestion is never left grey), and set the dim only while a
# suggestion is actually showing. syntax-highlight is sourced AFTER us (deterministic load order)
# and resets + repaints + re-applies this dim from POSTDISPLAY, so a themed command and the grey
# suggestion coexist; if syntax-highlight is absent, our dim still stands. Because we run FIRST,
# our reset never wipes the highlighter's colours.
autoload -Uz add-zle-hook-widget
typeset -g __tt_suggest=""
typeset -g __tt_suggest_done=""
__tt_autosuggest() {
  emulate -L zsh
  POSTDISPLAY=""
  __tt_suggest=""
  region_highlight=()
  # Line accepted (Enter): the FINAL redraw runs this hook one more time — a
  # recomputed ghost here would be stamped into the finished line and stay in
  # scrollback forever. Keep everything cleared until the next line starts.
  [[ -n $__tt_suggest_done ]] && return 0
  # IMPORTANT: always `return 0`. As a line-pre-redraw hook we share the chain with
  # syntax-highlight (which runs after us); a NON-zero return aborts the rest of the chain, so a
  # bare `return` here (no history match — e.g. `@ai`) would stop syntax-highlight from ever
  # painting the command. That's the bug behind "@ai has no colour".
  [[ -n $BUFFER && $CURSOR -eq ${#BUFFER} ]] || return 0
  local s=${history[(r)${(b)BUFFER}*]}
  [[ -n $s && $s != $BUFFER ]] || return 0
  __tt_suggest=${s#$BUFFER}
  POSTDISPLAY=$__tt_suggest
  region_highlight=("${#BUFFER} $(( ${#BUFFER} + ${#__tt_suggest} )) fg=$TT_MUTED")
  return 0
}
add-zle-hook-widget line-pre-redraw __tt_autosuggest
# Accepting a line (Enter) must ERASE an unaccepted ghost, not keep it: clear
# the suggestion state on line-finish (the redraw that follows wipes the cells)
# and re-arm on the next line-init.
__tt_autosuggest_finish() {
  __tt_suggest_done=1
  POSTDISPLAY=""
  __tt_suggest=""
  region_highlight=()
  return 0
}
__tt_autosuggest_init() { __tt_suggest_done=""; return 0 }
add-zle-hook-widget line-finish __tt_autosuggest_finish
add-zle-hook-widget line-init __tt_autosuggest_init
__tt_autosuggest_accept() {
  if [[ -n $__tt_suggest && $CURSOR -eq ${#BUFFER} ]]; then
    # Accept: the suggestion becomes real buffer text — drop its dim so it renders like a command.
    BUFFER=$BUFFER$__tt_suggest
    CURSOR=${#BUFFER}
    __tt_suggest=""; POSTDISPLAY=""; region_highlight=()
  else
    zle forward-char
  fi
}
zle -N __tt_autosuggest_accept
bindkey '^[[C' __tt_autosuggest_accept
bindkey '^[OC' __tt_autosuggest_accept
