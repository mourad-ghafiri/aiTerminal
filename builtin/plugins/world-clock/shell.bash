# Timezone helpers — tz/wclock/utc/unixtime.
utc()      { date -u '+%Y-%m-%d %H:%M:%S UTC'; }
unixtime() { date +%s; }
tz() {
  [ $# -gt 0 ] || { printf '%s\n' >&2 "usage: tz <Area/City> [Area/City ...]"; return 1; }
  local z
  for z in "$@"; do printf '%-24s %s\n' "$z" "$(TZ="$z" date '+%Y-%m-%d %H:%M')"; done
}
wclock() { tz UTC America/New_York Europe/London Europe/Paris Asia/Tokyo Australia/Sydney; }
