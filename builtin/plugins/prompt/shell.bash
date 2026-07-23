# Themed, git-aware bash prompt — only if you haven't set your own (the engine snapshots
# $PS1 into $__tt_ps1_default). Colors from the engine's TT_*_RGB vars.
if [ "$PS1" = "$__tt_ps1_default" ] && [ -z "$STARSHIP_SHELL" ]; then
  __tt_prompt() {
    local b mark git=""
    b=$(git symbolic-ref --short HEAD 2>/dev/null)
    if [ -n "$b" ]; then
      mark=""; git diff --quiet --ignore-submodules HEAD 2>/dev/null || mark=" ●"
      git=" \[\e[38;2;${TT_SUCCESS_RGB}m\]⎇ ${b}\[\e[38;2;${TT_WARN_RGB}m\]${mark}\[\e[0m\]"
    fi
    PS1="\[\e[38;2;${TT_ACCENT_RGB}m\]\w\[\e[0m\]${git} \[\e[38;2;${TT_ACCENT2_RGB}m\]❯\[\e[0m\] "
  }
  PROMPT_COMMAND="__tt_prompt"
fi
