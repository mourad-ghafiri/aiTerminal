# List the runnable scripts in the nearest package.json. Portable (identical body in shell.zsh).
nps() {
  [ -f package.json ] || { echo "nps: no package.json here" >&2; return 1; }
  if command -v jq >/dev/null 2>&1; then
    jq -r '.scripts // {} | to_entries[] | "  \(.key)  ->  \(.value)"' package.json
  else
    node -e 'const s=require("./package.json").scripts||{};for(const k in s)console.log("  "+k+"  ->  "+s[k])' 2>/dev/null \
      || command grep -A50 '"scripts"' package.json
  fi
}
