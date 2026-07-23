# kubectl output + rollout helpers. Portable (identical body in shell.bash).

# Run kubectl and pretty-print the result as JSON / YAML (uses jq / yq when installed).
kj() { kubectl "$@" -o json | { command -v jq >/dev/null 2>&1 && jq . || cat; }; }
ky() { kubectl "$@" -o yaml | { command -v yq >/dev/null 2>&1 && yq . || cat; }; }

# Force a rolling restart of a workload by stamping a changed env var: kres <type/name>...
kres() { kubectl set env "$@" "REFRESHED_AT=$(date +%Y%m%dT%H%M%S)"; }
