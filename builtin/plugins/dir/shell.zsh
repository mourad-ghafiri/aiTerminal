# Directory history + mkcd/take. Ctrl+Alt-←/→ walk the dirs you've visited this
# session, Alt-↑ goes up a level. (Alt-←/→ alone belong to word jumps — lineedit.)
# A lock flag stops programmatic back/forward jumps from corrupting the history
# that the chpwd hook records for normal `cd`s.
typeset -ga __tt_dh_back __tt_dh_fwd
typeset -g  __tt_dh_lock=""
__tt_dh_record() { [[ -n $__tt_dh_lock ]] && return; __tt_dh_back+=("$OLDPWD"); __tt_dh_fwd=(); }
autoload -Uz add-zsh-hook
add-zsh-hook chpwd __tt_dh_record
__tt_dir_back() {
  emulate -L zsh
  (( ${#__tt_dh_back} )) || return
  __tt_dh_fwd=("$PWD" "${__tt_dh_fwd[@]}")
  local d=${__tt_dh_back[-1]}; __tt_dh_back[-1]=()
  __tt_dh_lock=1; builtin cd -- "$d" 2>/dev/null; __tt_dh_lock=""
  zle reset-prompt
}
__tt_dir_fwd() {
  emulate -L zsh
  (( ${#__tt_dh_fwd} )) || return
  __tt_dh_back+=("$PWD")
  local d=${__tt_dh_fwd[1]}; __tt_dh_fwd[1]=()
  __tt_dh_lock=1; builtin cd -- "$d" 2>/dev/null; __tt_dh_lock=""
  zle reset-prompt
}
__tt_dir_up() { emulate -L zsh; builtin cd .. 2>/dev/null; zle reset-prompt; }
zle -N __tt_dir_back; zle -N __tt_dir_fwd; zle -N __tt_dir_up
bindkey '^[[1;7D' __tt_dir_back   # Ctrl+Alt-Left  → back
bindkey '^[[1;7C' __tt_dir_fwd    # Ctrl+Alt-Right → forward
bindkey '^[[1;3A' __tt_dir_up     # Alt-Up         → parent

mkcd() { mkdir -p -- "$@" && builtin cd -- "${@[-1]}"; }   # make a dir and step into it
take() { mkcd "$@"; }
