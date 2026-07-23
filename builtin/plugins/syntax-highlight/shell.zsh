# Real-time syntax highlighting via a line-pre-redraw ZLE hook. Sourced AFTER autosuggest
# (deterministic load order), so we run LAST and own the final region_highlight: reset it, paint
# our colors, then re-apply the autosuggest dim from POSTDISPLAY. autosuggest also sets its own
# dim (running before us), so the grey suggestion survives even if this plugin is disabled.
# region_highlight offsets are 0-based; zsh string indexing is 1-based.
__tt_highlight() {
  emulate -L zsh
  region_highlight=()
  local buf=$BUFFER n=${#BUFFER}
  (( n )) || return
  # 1) command word: first non-space token.
  local lead=${buf%%[^[:space:]]*} start
  start=${#lead}
  local rest=${buf:$start} first
  first=${rest%%[[:space:]]*}
  if [[ -n $first ]]; then
    # `@ai` / `@<agent>` is a first-class AI invocation (caught by the not-found handler,
    # so it is never a "known command"): theme it — the mention in accent-bold, the prompt
    # after it in accent2 — instead of the red "unknown command" colour.
    if [[ $first == @* ]]; then
      region_highlight+=("$start $(( start + ${#first} )) fg=$TT_ACCENT,bold")
      local pstart=$(( start + ${#first} ))
      (( pstart < n )) && region_highlight+=("$pstart $n fg=$TT_ACCENT2")
      [[ -n $POSTDISPLAY ]] && region_highlight+=("${#BUFFER} $(( ${#BUFFER} + ${#POSTDISPLAY} )) fg=$TT_MUTED")
      return 0
    fi
    # otherwise: green if known, red if not.
    local cc=$TT_ERROR
    whence -- $first &>/dev/null && cc=$TT_SUCCESS
    region_highlight+=("$start $(( start + ${#first} )) fg=$cc")
  fi
  # 2) one pass over the rest: quoted strings, flags, operators.
  local i=$(( start + ${#first} + 1 )) q="" qstart=0 ch prev
  while (( i <= n )); do
    ch=${buf[i]}; prev=${buf[i-1]}
    if [[ -n $q ]]; then
      [[ $ch == $q ]] && { region_highlight+=("$(( qstart - 1 )) $i fg=$TT_ACCENT2"); q="" }
    elif [[ $ch == "'" || $ch == '"' ]]; then
      q=$ch; qstart=$i
    elif [[ $ch == "-" && ( $prev == [[:space:]] || $i -eq 1 ) ]]; then
      local j=$i
      while (( j <= n )) && [[ ${buf[j]} != [[:space:]] ]]; do (( j++ )); done
      region_highlight+=("$(( i - 1 )) $(( j - 1 )) fg=$TT_WARN"); i=$j; continue
    elif [[ $ch == ("|"|"&"|";"|">"|"<") ]]; then
      region_highlight+=("$(( i - 1 )) $i fg=$TT_ACCENT")
    fi
    (( i++ ))
  done
  # 3) re-apply the autosuggest dim (we reset region_highlight above).
  [[ -n $POSTDISPLAY ]] && region_highlight+=("${#BUFFER} $(( ${#BUFFER} + ${#POSTDISPLAY} )) fg=$TT_MUTED")
  return 0  # never abort the line-pre-redraw hook chain (any hooks registered after us still run)
}
autoload -Uz add-zle-hook-widget
add-zle-hook-widget line-pre-redraw __tt_highlight
