# Cross-platform clipboard: copypath / copyfile / Ctrl-O (copy command line).
__tt_clipcmd() {
  if   (( $+commands[pbcopy] ));  then pbcopy
  elif (( $+commands[wl-copy] )); then wl-copy
  elif (( $+commands[xclip] ));   then xclip -selection clipboard
  elif (( $+commands[xsel] ));    then xsel --clipboard --input
  else cat >/dev/null; return 1; fi
}
copypath() { emulate -L zsh; local p=${1:-$PWD}; print -rn -- "${p:A}" | __tt_clipcmd && print "copied path: ${p:A}"; }
copyfile() { emulate -L zsh; [[ -f $1 ]] || { print -u2 "copyfile: no such file: $1"; return 1 }; __tt_clipcmd < "$1" && print "copied contents of $1"; }
# Read from the system clipboard (counterpart to copying).
__tt_pastecmd() {
  if   (( $+commands[pbpaste] ));  then pbpaste
  elif (( $+commands[wl-paste] )); then wl-paste
  elif (( $+commands[xclip] ));    then xclip -selection clipboard -o
  elif (( $+commands[xsel] ));     then xsel --clipboard --output
  else return 1; fi
}
# Pipe-friendly copy/paste: `echo hi | clip`, `clip < file`, `clippaste > out`.
clip()      { __tt_clipcmd; }
clippaste() { __tt_pastecmd; }
# Copy the most recent command line to the clipboard.
copyline()  { fc -ln -1 2>/dev/null | sed 's/^[[:space:]]*//' | __tt_clipcmd && print "copied last command"; }
__tt_copybuffer() { emulate -L zsh; print -rn -- "$BUFFER" | __tt_clipcmd; }
zle -N __tt_copybuffer
bindkey '^O' __tt_copybuffer
