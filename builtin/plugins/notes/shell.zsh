# Quick scratch notes at the shell — a timestamped plain-text file.
: ${TT_NOTES_FILE:=${HOME}/.aiTerminal/plugins/notes/quick.md}
note() {
  [ $# -gt 0 ] || { print -u2 "usage: note <text>"; return 1; }
  mkdir -p "${TT_NOTES_FILE:h}" 2>/dev/null
  printf -- '- %s  _(%s)_\n' "$*" "$(date '+%Y-%m-%d %H:%M')" >> "$TT_NOTES_FILE"
  print "noted."
}
notes()     { [ -s "$TT_NOTES_FILE" ] && cat "$TT_NOTES_FILE" || print "no notes yet — add one with: note <text>"; }
noteclear() { : > "$TT_NOTES_FILE" 2>/dev/null; print "notes cleared"; }
