# A first-class AI terminal. `command_not_found_handler` only runs when a typed
# command doesn't exist — and `@`-commands never do — so this NEVER intercepts a
# real command; normal typing is untouched. Everything is a terminal command:
#   @ai <request>         → a shell command (preloaded), or a prose answer for a question
#   @<agent> <task>       → run the named agent and print its answer (add --bg to track as a job)
#   @agent [<name>]       → the agents you have: tools, step cap, and what each returns
#   @flow <name> <input>  → run a workflow GRAPH (parallel branches, conditions, loops);
#                         bare @flow lists them; @flow check|graph <name> proves and draws one
#   @loop "<goal>" [--check "<cmd>"] → iterate an agent until the goal verifies;
#                         bare @loop lists recent runs, @loop resume <id> carries on
#   @job "<request>"      → say what to do and when; the AI reads the schedule once
#   @job -- <command>     → the same, as a command job (no model needed to run it)
#   @mcp                  → the declared MCP tool-servers, connected: era, tools, failures
#   @workspace            → open THIS folder as a conversation: /commands, @verbs inline,
#                         project .aiTerminal/ overlay behind a trust prompt
#   @md render <file>     → pretty-print a Markdown file (diagrams drawn natively)
#   @md edit <file>       → a live split editor: Markdown left, rendered preview right
#   @gate telegram start  → hand this pane to a chat app and drive it from your phone
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
    '#TT-ANSWER#'*)      # a prose answer already streamed to stderr — nothing to preload
      ;;
    \#*)                 # a refusal / guard block / error — shown, never run
      print -u2 -- "${out#\# }"
      ;;
  esac
}
typeset -ga precmd_functions
(( ${precmd_functions[(I)_tt_ai_load_pending]} )) || precmd_functions+=(_tt_ai_load_pending)

# ── @gate command marks ───────────────────────────────────────────────────────
# Only inside a shell spawned by `@gate` ($TT_GATE), report where each command
# starts and ends, so the gate knows exactly which output belongs to a command it
# was asked to run. Guessing from prompt text or output pauses is unreliable; this
# is two escape sequences the terminal swallows and nothing else ever sees.
if [[ -n ${TT_GATE:-} ]]; then
  _tt_gate_preexec() { printf '\033]1339;S\007' }
  # MUST be the first precmd: $? in a precmd is the previous FUNCTION's status once
  # another hook has run, so capture it before anything else can clobber it.
  _tt_gate_precmd()  { local s=$?; printf '\033]1339;E;%d\007' $s; return $s }
  typeset -ga preexec_functions precmd_functions
  (( ${preexec_functions[(I)_tt_gate_preexec]} )) || preexec_functions+=(_tt_gate_preexec)
  (( ${precmd_functions[(I)_tt_gate_precmd]} ))   || precmd_functions=(_tt_gate_precmd $precmd_functions)
fi
