# Docker convenience functions. Portable (identical body in shell.bash).

# Open an interactive shell inside a running container (bash, falling back to sh): dsh <name> [shell]
dsh() {
  [ -n "$1" ] || { echo "usage: dsh <container> [shell]" >&2; return 1; }
  if [ -n "$2" ]; then
    docker exec -it "$1" "$2"
  else
    docker exec -it "$1" bash 2>/dev/null || docker exec -it "$1" sh
  fi
}

# Remove stopped containers and dangling images.
dclean() { docker container prune -f && docker image prune -f; }

# Show a container's primary IP address: dip <container>
dip() {
  [ -n "$1" ] || { echo "usage: dip <container>" >&2; return 1; }
  docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$1"
}
