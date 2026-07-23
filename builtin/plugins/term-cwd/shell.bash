# Report cwd + host to the terminal via OSC 7 on every prompt, so the status bar reflects
# the live folder + user@host instantly (and the REMOTE path/host over SSH). Appends to
# PROMPT_COMMAND (this plugin sorts after `prompt`, so its value is preserved, not clobbered).
__tt_osc7() { printf '\033]7;file://%s%s\033\\' "${HOSTNAME:-}" "$PWD"; }
case ";${PROMPT_COMMAND};" in
  *";__tt_osc7;"*) ;;                                          # already wired
  *) PROMPT_COMMAND="${PROMPT_COMMAND:+$PROMPT_COMMAND;}__tt_osc7" ;;
esac
__tt_osc7   # seed the initial directory before the first prompt
