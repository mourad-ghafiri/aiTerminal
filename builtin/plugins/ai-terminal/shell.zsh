# A first-class AI terminal. `command_not_found_handler` only runs when a typed
# command doesn't exist — and `@`-commands never do — so this NEVER intercepts a
# real command; normal typing is untouched. Everything is a terminal command:
#   @ai <request>         → translate natural language to a shell command
#   @<agent> <task>       → run the named agent and print its answer (add --bg to track as a job)
#   @flow [<name> input]  → run a multi-step AI workflow; with no args, list them
#   @loop <goal> [--check "<cmd>"] [--max N] → iterate an agent until the goal verifies
#   @job [clear]          → list / prune background AI jobs
#   @profile [<id>]       → list / switch directly · create/rename/delete/edit ($EDITOR)
#   @theme [<name>]       → list themes / switch the current profile\x27s theme live
#   @config | @plugin …   → the matching aiTerminal subcommand
# All route through the offline-capable `aiTerminal` CLI (your key + redaction rules).
#
# `@ai <request>` mode comes from `[ai] mode` in config (default "manual"): manual
# preloads the command for review (Enter to run); auto runs a guard-allowed suggestion
# straight away. A guard-*confirm* command always drops to review. If the AI isn't
# configured/working, the error shows as a `#`-comment — never silence.
#
# NOTE: `command_not_found_handler` runs in a FORKED context, so a `cd`/`export`/`print -z`
# inside it cannot change the interactive shell. So `@ai <request>` writes ONE marker line
# to a per-session file, and the `precmd` hook below (which DOES run in the real shell)
# dispatches it — so a run/edit/comment all take effect in THIS shell (`cd`, `export`, …).
command_not_found_handler() {
  emulate -L zsh
  local cmd=$1
  shift
  case $cmd in
    @ai)
      [[ -n "$*" ]] || { print -u2 -- "usage: @ai <natural-language request>"; return 2 }
      # stdout (the ONE marker line) is captured for the precmd dispatcher; stderr
      # streams THROUGH — the CLI's live chrome (spinner, thinking, the command
      # forming, the token footer) plays right here while you wait.
      "${TT_BIN:-aiTerminal}" ai --command "$*" > "${TMPDIR:-/tmp}/tt-ai-pending.$$"
      return
      ;;
    @flow)
      local name=$1; shift 2>/dev/null
      if [[ -z $name ]]; then
        "${TT_BIN:-aiTerminal}" ai flow
      else
        "${TT_BIN:-aiTerminal}" ai --flow "$name" "$*"
      fi
      return
      ;;
    @loop)
      [[ -n "$*" ]] || { print -u2 -- 'usage: @loop "<goal>" [--check "<cmd>"] [--max N] [--budget TOKENS] [--agent <name>] [--bg]'; return 2 }
      "${TT_BIN:-aiTerminal}" ai --loop "$@"
      return
      ;;
    @job)     "${TT_BIN:-aiTerminal}" ai job "$@"; return ;;
    @profile) "${TT_BIN:-aiTerminal}" profile "$@"; return ;;
    @config)  "${TT_BIN:-aiTerminal}" config "$@"; return ;;
    @theme)   "${TT_BIN:-aiTerminal}" theme "$@"; return ;;
    @plugin)  "${TT_BIN:-aiTerminal}" plugin "$@"; return ;;
    @*)
      if [[ -n "$*" ]]; then
        "${TT_BIN:-aiTerminal}" ai --agent "${cmd#@}" "$@"
        return
      fi
      ;;
  esac
  print -u2 -- "zsh: command not found: $cmd"
  return 127
}

# Dispatch a pending `@ai` marker line. Runs in the REAL shell (precmd), so an auto-run
# `eval` and a preloaded `print -z` both take effect in this shell (`cd`/`export`/`source`).
_tt_ai_load_pending() {
  emulate -L zsh
  local f="${TMPDIR:-/tmp}/tt-ai-pending.$$"
  [[ -r $f ]] || return
  local out; out="$(<$f)"; command rm -f -- "$f" 2>/dev/null
  [[ -n $out ]] || return
  case $out in
    '#TT-RUN# '*)        # auto mode: run a guard-allowed command now
      local c=${out#\#TT-RUN# }
      print -P -u2 -- "%F{${TT_ACCENT:-39}}❯%f ${c}"
      eval "$c"
      ;;
    '#TT-EDIT# '*)       # manual mode: preload for review
      print -z -- "${out#\#TT-EDIT# }"
      print -P -u2 -- "%F{${TT_ACCENT:-39}}❯%f press Enter to run (or edit)"
      ;;
    '#TT-CONFIRM# '*)    # guard wants confirmation: preload with a warning
      print -z -- "${out#\#TT-CONFIRM# }"
      print -P -u2 -- "%F{${TT_WARN:-214}}⚠%f review before running (or edit)"
      ;;
    \#*)                 # a refusal / guard block / error — shown, never run
      print -u2 -- "${out#\# }"
      ;;
  esac
}
typeset -ga precmd_functions
(( ${precmd_functions[(I)_tt_ai_load_pending]} )) || precmd_functions+=(_tt_ai_load_pending)
