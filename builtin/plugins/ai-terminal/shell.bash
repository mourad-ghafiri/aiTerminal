# A first-class AI terminal (bash). `command_not_found_handle` only runs for an unknown
# command — and `@`-commands never exist — so a real command is NEVER intercepted.
# Everything is a terminal command:
#   @ai <request>         → translate natural language to a shell command
#   @<agent> <task>       → run the named agent and print its answer (add --bg to track as a job)
#   @agent [<name>]       → the agents you have: tools, step cap, and what each returns
#   @flow <name> <input>  → run a workflow GRAPH (parallel branches, conditions, loops);
#                         bare @flow lists them; @flow check|graph <name> proves and draws one
#   @loop "<goal>" [--check "<cmd>"] → iterate an agent until the goal verifies;
#                         bare @loop lists recent runs, @loop resume <id> carries on
#   @job "<request>"      → say what to do and when; the AI reads the schedule once
#   @job -- <command>     → the same, as a command job (no model needed to run it)
#   @md render|edit <file> → pretty-print a Markdown file, or live-edit it with a preview
#   @profile [<id>]       → list / switch directly · create/rename/delete/edit ($EDITOR)
#   @theme [<name>]       → list themes / switch the current profile\x27s theme live
#   @config | @plugin …   → the matching aiTerminal subcommand
#
# `@ai <request>` mode comes from `[ai] mode` in config (default "manual"): manual preloads
# the command for review (↑ then Enter); auto runs a guard-allowed suggestion straight away.
# A guard-*confirm* command always drops to review. If the AI isn't configured/working, the
# error shows as a `#`-comment — never silence.
#
# NOTE: `command_not_found_handle` runs in a FORKED context, so a `cd`/`export` inside it
# cannot change the interactive shell. So `@ai <request>` writes ONE marker line to a
# per-session file, and the PROMPT_COMMAND hook below (real shell) dispatches it — a
# run/preload/comment all take effect in THIS shell so `cd`/`export`/`source` persist.
command_not_found_handle() {
  local cmd=$1
  shift
  case "$cmd" in
    @ai)
      [ -n "$*" ] || { echo "usage: @ai <natural-language request>" >&2; return 2; }
      # stdout (the ONE marker line) is captured for the prompt-hook dispatcher;
      # stderr streams THROUGH — the CLI's live chrome plays right here.
      "${TT_BIN:-aiTerminal}" ai --command "$*" > "${TMPDIR:-/tmp}/tt-ai-pending.$$"
      return
      ;;
    @flow)    "${TT_BIN:-aiTerminal}" ai flow "$@"; return ;;
    @agent)   "${TT_BIN:-aiTerminal}" ai agent "$@"; return ;;
    @loop)    "${TT_BIN:-aiTerminal}" ai loop "$@"; return ;;
    @job)     "${TT_BIN:-aiTerminal}" ai job "$@"; return ;;
    @mcp)     "${TT_BIN:-aiTerminal}" ai mcp "$@"; return ;;
    @workspace) "${TT_BIN:-aiTerminal}" ai workspace "$@"; return ;;
    @md)      "${TT_BIN:-aiTerminal}" md "$@"; return ;;
    @gate)    "${TT_BIN:-aiTerminal}" gate "$@"; return ;;
    @profile) "${TT_BIN:-aiTerminal}" profile "$@"; return ;;
    @config)  "${TT_BIN:-aiTerminal}" config "$@"; return ;;
    @theme)   "${TT_BIN:-aiTerminal}" theme "$@"; return ;;
    @plugin)  "${TT_BIN:-aiTerminal}" plugin "$@"; return ;;
    @*)
      if [ -n "$*" ]; then
        "${TT_BIN:-aiTerminal}" ai --agent "${cmd#@}" "$@"
        return
      fi
      ;;
  esac
  echo "bash: $cmd: command not found" >&2
  return 127
}

# Dispatch a pending `@ai` marker line in the REAL shell (PROMPT_COMMAND), so an auto-run
# `eval` and a preloaded `history -s` both take effect here (`cd`/`export`/`source` persist).
_tt_ai_load_pending() {
  local f="${TMPDIR:-/tmp}/tt-ai-pending.$$"
  [ -r "$f" ] || return
  local out; out=$(cat "$f" 2>/dev/null); command rm -f -- "$f" 2>/dev/null
  [ -n "$out" ] || return
  local c
  case "$out" in
    '#TT-RUN# '*)        # auto mode: run a guard-allowed command now
      c="${out#'#TT-RUN# '}"; echo "❯ $c" >&2; eval "$c" ;;
    '#TT-EDIT# '*)       # manual mode: preload for review (↑ then Enter)
      c="${out#'#TT-EDIT# '}"; history -s -- "$c"; echo "❯ ↑ then Enter to run — $c" >&2 ;;
    '#TT-CONFIRM# '*)    # guard wants confirmation: preload with a warning
      c="${out#'#TT-CONFIRM# '}"; history -s -- "$c"; echo "⚠ review: ↑ then Enter — $c" >&2 ;;
    '#'*)                # a refusal / guard block / error — shown, never run
      echo "${out#\# }" >&2 ;;
  esac
}
case ";${PROMPT_COMMAND};" in
  *";_tt_ai_load_pending;"*) ;;
  *) PROMPT_COMMAND="_tt_ai_load_pending${PROMPT_COMMAND:+;$PROMPT_COMMAND}" ;;
esac

# ── @gate command marks ───────────────────────────────────────────────────────
# See the zsh notes. macOS ships bash 3.2, which has no PS0, so the "command is
# starting" mark rides on a DEBUG trap with a re-entrancy guard (the trap fires once
# per simple command, and again for each command inside a pipeline).
if [ -n "${TT_GATE:-}" ]; then
  _tt_gate_start() {
    [ -n "$_TT_GATE_IN" ] && return
    _TT_GATE_IN=1
    printf '\033]1339;S\007'
  }
  _tt_gate_end() {
    local s=$?
    [ -n "$_TT_GATE_IN" ] || return $s
    _TT_GATE_IN=
    printf '\033]1339;E;%d\007' "$s"
    return $s
  }
  trap '_tt_gate_start' DEBUG
  case ";${PROMPT_COMMAND};" in
    *";_tt_gate_end;"*) ;;
    *) PROMPT_COMMAND="_tt_gate_end${PROMPT_COMMAND:+;$PROMPT_COMMAND}" ;;
  esac
fi
