# Gentle hint (bash ≥ 4): when what you just ran has a shorter alias, show ONE dim line —
# the first time per alias per session only. The engine exports TT_ALIAS_BY_HEAD (expansion
# head token → longest-first `alias<TAB>expansion` rows). We suggest the LONGEST alias whose
# expansion is a token-prefix of the command you ran, regardless of args/quoting — so
# `git commit -m "msg"` → gcm, while `g status` (already short) is left alone. bash has no
# preexec, so we inspect the just-run history entry from PROMPT_COMMAND.
if ((BASH_VERSINFO[0] >= 4)); then
  declare -gA __tt_hinted
  declare -g __tt_hint_last
  { read -r __tt_hint_last _ < <(HISTTIMEFORMAT= history 1); } 2>/dev/null  # skip the pre-session line
  __tt_alias_hint() {
    local num rest
    read -r num rest <<< "$(HISTTIMEFORMAT= history 1)"
    [ -n "$rest" ] || return
    [ "$num" = "$__tt_hint_last" ] && return     # only consider each new command once
    __tt_hint_last=$num
    local -a toks; read -ra toks <<< "$rest"
    [ ${#toks[@]} -gt 0 ] || return
    local rows=${TT_ALIAS_BY_HEAD[${toks[0]}]}
    [ -n "$rows" ] || return
    local name exp i ok; local -a etoks
    # Rows are pre-sorted longest-first, so the first token-prefix match is the best one.
    while IFS=$'\t' read -r name exp; do
      [ -n "$name" ] || continue
      read -ra etoks <<< "$exp"
      [ ${#etoks[@]} -le ${#toks[@]} ] || continue
      ok=1
      for ((i = 0; i < ${#etoks[@]}; i++)); do
        [ "${toks[i]}" = "${etoks[i]}" ] || { ok=0; break; }
      done
      if [ "$ok" = 1 ]; then
        [ -n "${__tt_hinted[$name]}" ] && return
        __tt_hinted[$name]=1
        printf '\033[38;2;%sm💡 tip:\033[0m \033[38;2;%sm%s = %s\033[0m\n' \
          "${TT_ACCENT_RGB:-}" "${TT_MUTED_RGB:-}" "$name" "$exp" >&2
        return
      fi
    done <<< "$rows"
  }
  case ";${PROMPT_COMMAND};" in
    *";__tt_alias_hint;"*) ;;
    *) PROMPT_COMMAND="${PROMPT_COMMAND:+$PROMPT_COMMAND;}__tt_alias_hint" ;;
  esac
fi
