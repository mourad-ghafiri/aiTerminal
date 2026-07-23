# Frecency directory jumping (bash). PROMPT_COMMAND records every directory you enter;
# `j <terms>` cd's to the best frequency×recency match. Named marks: `mark [name]`,
# `unmark <name>`, `marks`, and `j @name`.
: "${TT_JUMP_DB:=$HOME/.aiTerminal/jump.db}"
: "${TT_MARKS_DB:=$HOME/.aiTerminal/marks.db}"
__tt_jump_prev="$PWD"
__tt_jump_add() {
  [ "$PWD" = "$__tt_jump_prev" ] && return
  __tt_jump_prev="$PWD"
  { [ "$PWD" = "$HOME" ] || [ "$PWD" = "/" ]; } && return
  mkdir -p "$(dirname "$TT_JUMP_DB")" 2>/dev/null
  local now tmp; now=$(date +%s); tmp="$TT_JUMP_DB.$$"
  { cat "$TT_JUMP_DB" 2>/dev/null; } | awk -F'|' -v d="$PWD" -v now="$now" '
    $3==d { print ($1+1)"|"now"|"$3; found=1; next } { print }
    END   { if (!found) print "1|"now"|"d }
  ' > "$tmp" && command mv -f -- "$tmp" "$TT_JUMP_DB"
}
case ";${PROMPT_COMMAND};" in
  *";__tt_jump_add;"*) ;;
  *) PROMPT_COMMAND="${PROMPT_COMMAND:+$PROMPT_COMMAND;}__tt_jump_add" ;;
esac

j() {
  local q="$*"
  if [ "${q#@}" != "$q" ]; then
    local m; m=$(awk -F'|' -v n="${q#@}" '$1==n{print $2; exit}' "$TT_MARKS_DB" 2>/dev/null)
    if [ -n "$m" ] && [ -d "$m" ]; then builtin cd -- "$m"; else echo "jump: no mark @${q#@}" >&2; return 1; fi
    return
  fi
  [ -z "$q" ] && { builtin cd -- "$HOME"; return; }
  local dir; dir=$(awk -F'|' -v now="$(date +%s)" -v q="$q" '
    { p=$3; ok=1; n=split(q,t," ");
      for (i=1;i<=n;i++) if (index(tolower(p),tolower(t[i]))==0) { ok=0; break }
      if (!ok) next;
      dt=now-$2; w=$1;
      if (dt<3600) w*=4; else if (dt<86400) w*=2; else if (dt<604800) w*=1; else w*=0.25;
      if (w>best) { best=w; bp=p } }
    END { if (bp) print bp }' "$TT_JUMP_DB" 2>/dev/null)
  if [ -n "$dir" ] && [ -d "$dir" ]; then builtin cd -- "$dir"; else echo "jump: no match for '$q'" >&2; return 1; fi
}

mark()   { local n="${1:-${PWD##*/}}"; mkdir -p "$(dirname "$TT_MARKS_DB")" 2>/dev/null; printf '%s|%s\n' "$n" "$PWD" >> "$TT_MARKS_DB"; echo "marked $PWD as @$n"; }
unmark() { [ -f "$TT_MARKS_DB" ] || return; local tmp="$TT_MARKS_DB.$$"; awk -F'|' -v n="$1" '$1!=n' "$TT_MARKS_DB" > "$tmp" && command mv -f -- "$tmp" "$TT_MARKS_DB"; }
marks()  { if [ -f "$TT_MARKS_DB" ]; then awk -F'|' '{printf "  @%-14s %s\n",$1,$2}' "$TT_MARKS_DB"; else echo "no marks yet — use: mark [name]"; fi; }
