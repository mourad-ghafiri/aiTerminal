# Gentle hint: when what you typed has a shorter alias, show ONE dim line — the first time
# per alias per session only (never naggy). The engine exports TT_ALIAS_BY_HEAD (expansion
# head token → longest-first `alias<TAB>expansion` rows). We suggest the LONGEST alias whose
# expansion is a token-prefix of the command you typed, regardless of args/quoting — so
# `git commit -m "msg"` → gcm, `git status --short` → gst, and `g status` (already short) is
# left alone.
typeset -gA __tt_hinted
__tt_alias_hint() {
  emulate -L zsh
  local -a toks; toks=( ${(z)1} )
  (( ${#toks} )) || return
  local rows=${TT_ALIAS_BY_HEAD[$toks[1]]}
  [[ -n $rows ]] || return
  local row name exp i; local -a etoks
  # Rows are pre-sorted longest-first, so the first token-prefix match is the best one.
  for row in ${(f)rows}; do
    name=${row%%$'\t'*}; exp=${row#*$'\t'}
    etoks=( ${(z)exp} )
    (( ${#etoks} <= ${#toks} )) || continue
    for (( i = 1; i <= ${#etoks}; i++ )); do
      [[ $toks[i] == $etoks[i] ]] || { name=""; break }
    done
    if [[ -n $name ]]; then
      [[ -n ${__tt_hinted[$name]} ]] && return
      __tt_hinted[$name]=1
      print -P -u2 -- "%F{$TT_ACCENT}💡 tip:%f %F{$TT_MUTED}$name = $exp%f"
      return
    fi
  done
}
autoload -Uz add-zsh-hook
add-zsh-hook preexec __tt_alias_hint
