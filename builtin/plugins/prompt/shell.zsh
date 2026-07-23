# Themed, git-aware shell prompt — installed ONLY if you haven't set your own (the
# engine snapshots $PROMPT into $__tt_prompt_default before sourcing your rc) and never
# over a known framework. Colors come from the theme via the engine's TT_* vars.
if [[ "$PROMPT" == "$__tt_prompt_default" && -z "$ZSH" && -z "$STARSHIP_SHELL" && -z "$POWERLEVEL9K_MODE" && -z "$P9K_SSH" ]]; then
  autoload -Uz vcs_info add-zsh-hook
  zstyle ':vcs_info:*' enable git
  zstyle ':vcs_info:git:*' check-for-changes true
  zstyle ':vcs_info:git:*' unstagedstr ' ●'
  zstyle ':vcs_info:git:*' stagedstr ' ✚'
  zstyle ':vcs_info:git:*' formats " %F{$TT_SUCCESS}⎇ %b%f%F{$TT_WARN}%u%c%f"
  zstyle ':vcs_info:git:*' actionformats " %F{$TT_WARN}⎇ %b|%a%f"
  add-zsh-hook precmd vcs_info
  setopt prompt_subst
  PROMPT="%F{$TT_ACCENT}%~%f"'${vcs_info_msg_0_}'" %F{$TT_ACCENT2}❯%f "
fi
