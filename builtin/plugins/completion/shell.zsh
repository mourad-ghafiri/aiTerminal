# Intelligent completion. With complete_aliases OFF, zsh expands an alias before
# completing — so `gco <Tab>` completes branches and `g <Tab>` completes git, for free.
autoload -Uz compinit
compinit -C
zmodload -i zsh/complist 2>/dev/null
unsetopt complete_aliases
zstyle ':completion:*' menu select
zstyle ':completion:*' matcher-list 'm:{a-zA-Z}={A-Za-z}' 'r:|[._-]=* r:|=*'
zstyle ':completion:*' group-name ''
zstyle ':completion:*:descriptions' format "%F{$TT_MUTED}%d%f"
zstyle ':completion:*' list-colors "${(s.:.)LS_COLORS}"
zstyle ':completion:*' rehash true

# Declarative completions: the engine exports plugins' [[completion]] specs as
# TT_COMPL_SUB / TT_COMPL_FLAGS (command → space-joined subcommands / flags). Register a
# single generic completer for each, so a plugin adds tab-completion for any custom tool
# with pure data — no _foo() zsh authoring.
if (( ${+TT_COMPL_SUB} )); then
  __tt_declarative_complete() {
    local cmd=${words[1]}
    local -a subs flags
    subs=(${=TT_COMPL_SUB[$cmd]})
    flags=(${=TT_COMPL_FLAGS[$cmd]})
    (( ${#subs}  )) && _describe -t subcommands 'subcommand' subs
    (( ${#flags} )) && _describe -t flags 'flag' flags
  }
  for __tt_c in ${(k)TT_COMPL_SUB}; do compdef __tt_declarative_complete "$__tt_c"; done
  unset __tt_c
fi
