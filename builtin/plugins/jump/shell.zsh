# Frecency directory jumping. The chpwd hook records every directory (score|epoch|path);
# `j <terms>` ranks by frequency weighted by recency and cd's to the best match. Named
# marks live in a second file: `mark [name]`, `unmark <name>`, `marks`, and `j @name`.
zmodload zsh/datetime 2>/dev/null
: ${TT_JUMP_DB:=${HOME}/.aiTerminal/jump.db}
: ${TT_MARKS_DB:=${HOME}/.aiTerminal/marks.db}

__tt_jump_add() {
  emulate -L zsh
  local d=$PWD
  [[ $d == $HOME || $d == / ]] && return
  mkdir -p ${TT_JUMP_DB:h} 2>/dev/null
  local tmp=$TT_JUMP_DB.$$
  { cat $TT_JUMP_DB 2>/dev/null } | awk -F'|' -v d="$d" -v now="$EPOCHSECONDS" '
    $3==d { print ($1+1)"|"now"|"$3; found=1; next } { print }
    END   { if (!found) print "1|"now"|"d }
  ' > $tmp && command mv -f -- $tmp $TT_JUMP_DB
}
autoload -Uz add-zsh-hook
add-zsh-hook chpwd __tt_jump_add

j() {
  emulate -L zsh
  local q=$*
  if [[ $q == @* ]]; then
    local m=$(awk -F'|' -v n="${q#@}" '$1==n{print $2; exit}' $TT_MARKS_DB 2>/dev/null)
    [[ -n $m && -d $m ]] && builtin cd -- "$m" || { print -u2 "jump: no mark @${q#@}"; return 1 }
    return
  fi
  [[ -z $q ]] && { builtin cd -- "$HOME"; return }
  local dir=$(awk -F'|' -v now="$EPOCHSECONDS" -v q="$q" '
    { p=$3; ok=1; n=split(q,t," ");
      for (i=1;i<=n;i++) if (index(tolower(p),tolower(t[i]))==0) { ok=0; break }
      if (!ok) next;
      dt=now-$2; w=$1;
      if (dt<3600) w*=4; else if (dt<86400) w*=2; else if (dt<604800) w*=1; else w*=0.25;
      if (w>best) { best=w; bp=p } }
    END { if (bp) print bp }' $TT_JUMP_DB 2>/dev/null)
  [[ -n $dir && -d $dir ]] && builtin cd -- "$dir" || { print -u2 "jump: no match for '$q'"; return 1 }
}

mark()   { emulate -L zsh; local n=${1:-${PWD:t}}; mkdir -p ${TT_MARKS_DB:h} 2>/dev/null; print -r -- "$n|$PWD" >> $TT_MARKS_DB; print "marked $PWD as @$n"; }
unmark() { emulate -L zsh; [[ -f $TT_MARKS_DB ]] || return; local tmp=$TT_MARKS_DB.$$; awk -F'|' -v n="$1" '$1!=n' $TT_MARKS_DB > $tmp && command mv -f -- $tmp $TT_MARKS_DB; }
marks()  { emulate -L zsh; [[ -f $TT_MARKS_DB ]] && awk -F'|' '{printf "  @%-14s %s\n",$1,$2}' $TT_MARKS_DB || print "no marks yet — use: mark [name]"; }
