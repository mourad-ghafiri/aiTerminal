# Optional free-form shell feature code — sourced only for TRUSTED plugins
# (builtin, or installed by you). Theme colors ride in as $TT_* vars.
hello_time() {
  print -P "%F{${TT_ACCENT:-39}}✦%f it is $(date +%H:%M)"
}
