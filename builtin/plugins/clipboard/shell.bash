# Cross-platform clipboard: copypath / copyfile / Ctrl-O (copy command line, bash ≥ 4).
__tt_clipcmd() {
  if   command -v pbcopy  >/dev/null 2>&1; then pbcopy
  elif command -v wl-copy >/dev/null 2>&1; then wl-copy
  elif command -v xclip   >/dev/null 2>&1; then xclip -selection clipboard
  elif command -v xsel    >/dev/null 2>&1; then xsel --clipboard --input
  else cat >/dev/null; return 1; fi
}
copypath() {
  local p="${1:-$PWD}"
  if command -v realpath >/dev/null 2>&1; then p=$(realpath -- "$p" 2>/dev/null || echo "$p"); fi
  printf '%s' "$p" | __tt_clipcmd && echo "copied path: $p"
}
copyfile() { [ -f "$1" ] || { echo "copyfile: no such file: $1" >&2; return 1; }; __tt_clipcmd < "$1" && echo "copied contents of $1"; }
# Read from the system clipboard (counterpart to copying).
__tt_pastecmd() {
  if   command -v pbpaste  >/dev/null 2>&1; then pbpaste
  elif command -v wl-paste >/dev/null 2>&1; then wl-paste
  elif command -v xclip    >/dev/null 2>&1; then xclip -selection clipboard -o
  elif command -v xsel     >/dev/null 2>&1; then xsel --clipboard --output
  else return 1; fi
}
# Pipe-friendly copy/paste: `echo hi | clip`, `clip < file`, `clippaste > out`.
clip()      { __tt_clipcmd; }
clippaste() { __tt_pastecmd; }
# Copy the most recent command line to the clipboard.
copyline()  { fc -ln -1 2>/dev/null | sed 's/^[[:space:]]*//' | __tt_clipcmd && echo "copied last command"; }
if ((BASH_VERSINFO[0] >= 4)); then
  __tt_copybuffer() { printf '%s' "$READLINE_LINE" | __tt_clipcmd; }
  bind -x '"\C-o":__tt_copybuffer' 2>/dev/null
fi
