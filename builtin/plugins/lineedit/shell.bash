# macOS-style command-line navigation (bash / readline). The app encodes
# modified keys as xterm CSI 1;<mod> sequences; navigation and kills map 1:1.
# Readline has no selection region, so the ⇧-selection family is zsh-only
# (see shell.zsh) — bash still gets word/line jumps and the big deletes.
bind '"\e[1;3D": backward-word' 2>/dev/null        # ⌥←
bind '"\e[1;3C": forward-word' 2>/dev/null         # ⌥→
bind '"\e[1;5D": backward-word' 2>/dev/null        # ⌃←
bind '"\e[1;5C": forward-word' 2>/dev/null         # ⌃→
bind '"\e[1;9D": beginning-of-line' 2>/dev/null    # ⌘←
bind '"\e[1;9C": end-of-line' 2>/dev/null          # ⌘→
bind '"\e[1;9A": beginning-of-line' 2>/dev/null    # ⌘↑ (single-line buffer)
bind '"\e[1;9B": end-of-line' 2>/dev/null          # ⌘↓
bind '"\e\C-?": backward-kill-word' 2>/dev/null    # ⌥⌫
bind '"\e[127;9u": backward-kill-line' 2>/dev/null # ⌘⌫
