# Esc Esc → toggle a `sudo ` prefix on the command line (or the previous command if the
# line is empty). Needs bash ≥ 4 (READLINE_LINE editing in a bind -x widget).
if ((BASH_VERSINFO[0] >= 4)); then
  __tt_sudo_toggle() {
    if [ -z "$READLINE_LINE" ]; then
      local last; last=$(fc -ln -1 2>/dev/null)
      READLINE_LINE=${last#"${last%%[![:space:]]*}"}   # ltrim
      READLINE_POINT=${#READLINE_LINE}
    fi
    if [ "${READLINE_LINE:0:5}" = "sudo " ]; then
      READLINE_LINE=${READLINE_LINE:5}
      READLINE_POINT=$(( READLINE_POINT >= 5 ? READLINE_POINT - 5 : 0 ))
    else
      READLINE_LINE="sudo $READLINE_LINE"
      READLINE_POINT=$(( READLINE_POINT + 5 ))
    fi
  }
  bind -x '"\e\e":__tt_sudo_toggle' 2>/dev/null
fi
