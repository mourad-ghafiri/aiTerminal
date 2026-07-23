# Esc Esc → toggle a `sudo ` prefix on the command line (or the previous command if the
# line is empty). A second press removes it.
__tt_sudo_toggle() {
  emulate -L zsh
  if [[ -z $BUFFER ]]; then
    local last=$(fc -ln -1 2>/dev/null)
    BUFFER=${last#"${last%%[![:space:]]*}"}   # ltrim
    CURSOR=${#BUFFER}
  fi
  if [[ $BUFFER == "sudo "* ]]; then
    BUFFER=${BUFFER#sudo }
    (( CURSOR > 5 ? (CURSOR -= 5) : (CURSOR = 0) ))
  else
    BUFFER="sudo $BUFFER"
    (( CURSOR += 5 ))
  fi
}
zle -N __tt_sudo_toggle
bindkey '\e\e' __tt_sudo_toggle
