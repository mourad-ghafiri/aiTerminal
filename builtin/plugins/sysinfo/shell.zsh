# `sysinfo` — a one-shot system dashboard (OS, uptime, load, disk, memory). Portable across
# macOS and Linux; the same body ships for bash.
sysinfo() {
  printf '🖥  %s\n' "$(uname -srm)"
  printf '⏱  uptime: %s\n' "$(uptime | sed -E 's/.*up[[:space:]]+//; s/,[[:space:]]*[0-9]+ user.*//; s/[[:space:]]*load average.*//')"
  printf '⚙  load:   %s\n' "$(uptime | sed -E 's/.*load averages?:[[:space:]]*//')"
  printf '💾  disk:   %s\n' "$(df -h / | awk 'NR==2{print $3" used / "$2" ("$5")"}')"
  if command -v free >/dev/null 2>&1; then
    printf '🧠  mem:    %s\n' "$(free -h | awk 'NR==2{print $3" / "$2}')"
  elif command -v vm_stat >/dev/null 2>&1; then
    printf '🧠  mem:    %s\n' "$(vm_stat | awk '/Pages active/{a=$3} /Pages wired/{w=$4} END{printf "%.1f GB active+wired", (a+w)*4096/1073741824}')"
  fi
}
