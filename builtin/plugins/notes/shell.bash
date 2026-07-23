# Quick scratch notes at the shell — a timestamped plain-text file.
: "${TT_NOTES_FILE:=$HOME/.aiTerminal/plugins/notes/quick.md}"
note() {
  [ $# -gt 0 ] || { echo "usage: note <text>" >&2; return 1; }
  mkdir -p "$(dirname "$TT_NOTES_FILE")" 2>/dev/null
  printf -- '- %s  _(%s)_\n' "$*" "$(date '+%Y-%m-%d %H:%M')" >> "$TT_NOTES_FILE"
  echo "noted."
}
notes()     { if [ -s "$TT_NOTES_FILE" ]; then cat "$TT_NOTES_FILE"; else echo "no notes yet — add one with: note <text>"; fi; }
noteclear() { : > "$TT_NOTES_FILE" 2>/dev/null; echo "notes cleared"; }
