# Clone a GitHub repo (owner/name or URL) with gh and cd into it. Portable.
ghcd() {
  [ -n "$1" ] || { echo "usage: ghcd <owner/repo | url>" >&2; return 1; }
  gh repo clone "$1" || return
  cd "$(basename "${1%.git}")"
}
