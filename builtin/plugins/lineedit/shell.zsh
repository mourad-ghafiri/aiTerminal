# macOS-style navigation & selection on the command line. The app encodes
# modified keys as the standard xterm CSI 1;<mod> sequences (Shift=2 Alt=3
# Shift+Alt=4 Ctrl=5 Cmd=9 Shift+Cmd=10); this plugin gives them meaning.
#
# STABILITY CONTRACT: while nothing is selected the shell is 100% stock — no
# global widget (self-insert, backspace, …) is replaced, so typing, Ctrl-R
# isearch, completion menus and every other plugin behave exactly as without
# this plugin. All selection behavior lives in a separate `tt-select` keymap
# that is entered on the first ⇧-move and left the moment anything else runs.
autoload -Uz add-zle-hook-widget add-zsh-hook

# ── navigate: ⌥/⌃ ←/→ by word, ⌘←/→ line ends, ⌘↑/↓ buffer ends ─────────────
bindkey '^[[1;3D' backward-word                   # ⌥←
bindkey '^[[1;3C' forward-word                    # ⌥→
bindkey '^[[1;5D' backward-word                   # ⌃←  (terminal habit)
bindkey '^[[1;5C' forward-word                    # ⌃→
bindkey '^[[1;9D' beginning-of-line               # ⌘←
bindkey '^[[1;9C' end-of-line                     # ⌘→
bindkey '^[[1;9A' beginning-of-buffer-or-history  # ⌘↑
bindkey '^[[1;9B' end-of-buffer-or-history        # ⌘↓

# ── edit: ⌥⌫ kills the word, ⌘⌫ kills to the line start ─────────────────────
bindkey '^[^?' backward-kill-word                 # ⌥⌫ (zsh default, kept explicit)
bindkey '^[[127;9u' backward-kill-line            # ⌘⌫ (CSI-u form from the app)

# ── selection color: ONE uniform background, text keeps its colors ──────────
# zsh's default region highlight is standout (reverse video): every selected
# cell inverts its OWN color, so a syntax-highlighted command selects as a
# patchwork of colored blocks. Paint the region with the theme's selection
# background instead (TT_SEL_BG from colors.sh — a precmd keeps it fresh
# across live theme switches; the other entries are zsh's defaults).
__tt_le_colors() {
  [[ -n $TT_SEL_BG ]] || return 0
  zle_highlight=("region:bg=$TT_SEL_BG" special:standout suffix:bold isearch:underline paste:standout)
}
__tt_le_colors
add-zsh-hook precmd __tt_le_colors

# ── select: ⇧ + a movement starts/extends the region ────────────────────────
# The first ⇧-move drops the mark and switches to the tt-select keymap; every
# further ⇧-move stretches the region. All widgets share the __tt_le_sel_
# prefix — the sync hook below recognizes them by it.
# __tt_le_ready is set once the tt-select keymap exists (first prompt); until
# then ⇧-select still works in the main keymap, just without replace-on-type.
__tt_le_sel_start()  { (( REGION_ACTIVE )) || { zle .set-mark-command; (( __tt_le_ready )) && zle -K tt-select; } }
__tt_le_sel_left()   { __tt_le_sel_start; zle .backward-char; }
__tt_le_sel_right()  { __tt_le_sel_start; zle .forward-char; }
__tt_le_sel_wleft()  { __tt_le_sel_start; zle .backward-word; }
__tt_le_sel_wright() { __tt_le_sel_start; zle .forward-word; }
__tt_le_sel_home()   { __tt_le_sel_start; zle .beginning-of-line; }
__tt_le_sel_end()    { __tt_le_sel_start; zle .end-of-line; }
zle -N __tt_le_sel_left;  zle -N __tt_le_sel_right
zle -N __tt_le_sel_wleft; zle -N __tt_le_sel_wright
zle -N __tt_le_sel_home;  zle -N __tt_le_sel_end
bindkey '^[[1;2D'  __tt_le_sel_left    # ⇧←    char
bindkey '^[[1;2C'  __tt_le_sel_right   # ⇧→    char
bindkey '^[[1;4D'  __tt_le_sel_wleft   # ⇧⌥←   word
bindkey '^[[1;4C'  __tt_le_sel_wright  # ⇧⌥→   word
bindkey '^[[1;6D'  __tt_le_sel_wleft   # ⇧⌃←   word
bindkey '^[[1;6C'  __tt_le_sel_wright  # ⇧⌃→   word
bindkey '^[[1;10D' __tt_le_sel_home    # ⇧⌘←   to line start
bindkey '^[[1;10C' __tt_le_sel_end     # ⇧⌘→   to line end

# ── ⌘C copies the keyboard selection ────────────────────────────────────────
# With no mouse selection the app forwards ⌘C as a CSI-u sequence; this widget
# sends the region to the system clipboard via OSC 52 (the app applies it).
# The name carries the __tt_le_sel_ prefix ON PURPOSE: the sync hook then
# KEEPS the selection alive after a copy, like every macOS text field.
__tt_le_sel_copy() {
  emulate -L zsh
  (( REGION_ACTIVE )) || return 0
  local a=$MARK b=$CURSOR t
  (( a > b )) && { t=$a; a=$b; b=$t; }
  local sel=${BUFFER[a+1,b]}
  [[ -n $sel ]] || return 0
  print -rn -- $'\e]52;c;'"$(print -rn -- "$sel" | base64 | tr -d '\n')"$'\a' > /dev/tty
}
zle -N __tt_le_sel_copy
bindkey '^[[99;9u' __tt_le_sel_copy

# ── the selection keymap: only active WHILE a region is live ────────────────
__tt_le_sel_replace() { zle .kill-region; zle .self-insert; }  # type over it
__tt_le_sel_kill()    { zle .kill-region; }                    # ⌫/⌦ delete it
__tt_le_sel_cancel()  { REGION_ACTIVE=0; zle -K main; }        # Esc drops it
__tt_le_noop()        { :; }                                   # swallowed sequence
zle -N __tt_le_sel_replace; zle -N __tt_le_sel_kill
zle -N __tt_le_sel_cancel; zle -N __tt_le_noop

# Built at the FIRST prompt — after the whole integration, every other plugin
# and the user's own zshrc have finished binding keys — so tt-select is a
# faithful snapshot of main plus our overrides, and we never clobber anyone.
__tt_le_setup() {
  emulate -L zsh
  add-zsh-hook -d precmd __tt_le_setup
  # Swallow modified sequences nobody bound (⌘Home, ⇧⌘↑, …): without this an
  # unhandled `ESC [1;10H` half-matches and sprays `0H` into the buffer. Only
  # truly undefined sequences are touched — never another plugin's binding.
  # NOTE the quoted letters: common's GLOBAL aliases (H = `| head`, C = `| wc -l`)
  # expand even here during parse — a bare H would become a pipe and kill the
  # whole integration file with a parse error.
  local m k
  for m in 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16; do
    for k in 'A' 'B' 'C' 'D' 'F' 'H'; do
      [[ $(bindkey "^[[1;${m}${k}") == *undefined-key* ]] && bindkey "^[[1;${m}${k}" __tt_le_noop
    done
    for k in 2 5 6; do
      [[ $(bindkey "^[[${k};${m}~") == *undefined-key* ]] && bindkey "^[[${k};${m}~" __tt_le_noop
    done
    # a modified forward-delete still deletes
    [[ $(bindkey "^[[3;${m}~") == *undefined-key* ]] && bindkey "^[[3;${m}~" delete-char
  done
  # the selection keymap: a snapshot of main + region-aware overrides
  bindkey -D tt-select 2>/dev/null
  bindkey -N tt-select main
  bindkey -M tt-select -R ' '-'~' __tt_le_sel_replace  # typing replaces the selection
  bindkey -M tt-select '^?'    __tt_le_sel_kill        # ⌫ deletes it
  bindkey -M tt-select '^[[3~' __tt_le_sel_kill        # ⌦ deletes it
  # Esc cancels the selection. A bare '^[' binding still lets longer ESC-
  # prefixed sequences (arrows, ⌥-keys) match first — zsh resolves the
  # ambiguity with $KEYTIMEOUT, exactly like the sudo plugin's Esc-Esc.
  bindkey -M tt-select '^[' __tt_le_sel_cancel
  # The band-under-colored-text painter (see __tt_le_paint) must run AFTER
  # syntax-highlight's repaint, and hooks fire in registration order — so it
  # registers HERE, at the first prompt, when every plugin's hooks exist.
  add-zle-hook-widget line-pre-redraw __tt_le_paint
  typeset -g __tt_le_ready=1
}
add-zsh-hook precmd __tt_le_setup

# ── the selection band under COLORED text ───────────────────────────────────
# zle_highlight's region:bg covers plain cells, but syntax-highlight paints
# its colors through region_highlight — and those per-character entries
# override the region's background, so a colored command showed the band
# only on its spaces. This hook (registered LAST, in __tt_le_setup) appends
# one bg-only entry over the selection after every repaint: zsh merges the
# attributes, so the text keeps its syntax colors ON the band.
__tt_le_paint() {
  emulate -L zsh
  if (( REGION_ACTIVE )) && [[ -n $TT_SEL_BG ]]; then
    local a=$MARK b=$CURSOR t
    (( a > b )) && { t=$a; a=$b; b=$t; }
    (( b > a )) && region_highlight+=("$a $b bg=$TT_SEL_BG")
  fi
  return 0  # never break the line-pre-redraw chain
}

# ── leave selection mode the moment anything else happens ───────────────────
# macOS behavior: any non-⇧ action drops the region. The copied binding in
# tt-select already DID the right thing (moved, searched history, completed…);
# this hook just clears the region and returns to the stock keymap. It never
# rebinds plain arrows, so history ↑/↓ and autosuggest → stay untouched.
__tt_le_sync() {
  emulate -L zsh
  if [[ $KEYMAP == tt-select ]]; then
    if (( ! REGION_ACTIVE )) || [[ $LASTWIDGET != __tt_le_sel_* ]]; then
      REGION_ACTIVE=0
      zle -K main
    fi
  fi
  return 0  # never break the line-pre-redraw chain (autosuggest / syntax-highlight)
}
add-zle-hook-widget line-pre-redraw __tt_le_sync
# belt & braces: every new line starts unselected, in the stock keymap
__tt_le_line_init() { REGION_ACTIVE=0; [[ $KEYMAP == tt-select ]] && zle -K main; return 0 }
add-zle-hook-widget line-init __tt_le_line_init
