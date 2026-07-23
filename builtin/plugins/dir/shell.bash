# Directory history + mkcd/take (bash). Alt-←/→ walk the dirs you've visited this session,
# Alt-↑ goes up a level. bash has no chpwd hook, so PROMPT_COMMAND records each move.
__tt_dh_back=(); __tt_dh_fwd=(); __tt_dh_prev="$PWD"
__tt_dh_record() {
  [ "$PWD" = "$__tt_dh_prev" ] && return
  __tt_dh_back+=("$__tt_dh_prev"); __tt_dh_fwd=(); __tt_dh_prev="$PWD"
}
case ";${PROMPT_COMMAND};" in
  *";__tt_dh_record;"*) ;;
  *) PROMPT_COMMAND="${PROMPT_COMMAND:+$PROMPT_COMMAND;}__tt_dh_record" ;;
esac
__tt_dir_back() {
  local n=${#__tt_dh_back[@]}; [ "$n" -gt 0 ] || return
  __tt_dh_fwd=("$PWD" "${__tt_dh_fwd[@]}")
  builtin cd -- "${__tt_dh_back[n-1]}" 2>/dev/null
  unset '__tt_dh_back[n-1]'; __tt_dh_back=("${__tt_dh_back[@]}"); __tt_dh_prev="$PWD"
}
__tt_dir_fwd() {
  [ "${#__tt_dh_fwd[@]}" -gt 0 ] || return
  __tt_dh_back+=("$PWD")
  builtin cd -- "${__tt_dh_fwd[0]}" 2>/dev/null
  __tt_dh_fwd=("${__tt_dh_fwd[@]:1}"); __tt_dh_prev="$PWD"
}
__tt_dir_up() { builtin cd .. 2>/dev/null; }
bind -x '"\e[1;3D":__tt_dir_back' 2>/dev/null   # Alt-Left  → back
bind -x '"\e[1;3C":__tt_dir_fwd'  2>/dev/null   # Alt-Right → forward
bind -x '"\e[1;3A":__tt_dir_up'   2>/dev/null   # Alt-Up    → parent

mkcd() { mkdir -p -- "$@" && builtin cd -- "${!#}"; }   # make a dir and step into it
take() { mkcd "$@"; }
