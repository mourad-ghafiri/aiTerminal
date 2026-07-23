# Branch-resolution helpers + power functions backing the git aliases. Written portably so
# the same body runs under zsh and bash (see shell.bash — identical). Aliases that interpolate
# `$(git_main_branch)` etc. resolve through these at call time.

# The current branch name (or short SHA when detached).
git_current_branch() {
  local ref
  ref=$(git symbolic-ref --quiet HEAD 2>/dev/null) || ref=$(git rev-parse --short HEAD 2>/dev/null) || return
  echo "${ref#refs/heads/}"
}

# The repository's primary branch: the remote default when known, else the first conventional
# name that exists, else "main".
git_main_branch() {
  git rev-parse --git-dir >/dev/null 2>&1 || return
  local ref b
  ref=$(git symbolic-ref --quiet refs/remotes/origin/HEAD 2>/dev/null) && { echo "${ref##*/}"; return; }
  for b in main trunk master; do
    if git show-ref -q --verify "refs/heads/$b" 2>/dev/null || git show-ref -q --verify "refs/remotes/origin/$b" 2>/dev/null; then
      echo "$b"; return
    fi
  done
  echo main
}

# The development branch by convention, else "develop".
git_develop_branch() {
  git rev-parse --git-dir >/dev/null 2>&1 || return
  local b
  for b in dev devel develop development; do
    if git show-ref -q --verify "refs/heads/$b" 2>/dev/null; then echo "$b"; return; fi
  done
  echo develop
}

# Rename a branch locally and on origin: grename [<old>] <new>.
grename() {
  if [ -z "$1" ]; then echo "usage: grename [<old>] <new>" >&2; return 1; fi
  local old new
  if [ -z "$2" ]; then old=$(git_current_branch); new=$1; else old=$1; new=$2; fi
  git branch -m "$old" "$new" || return
  if git rev-parse --abbrev-ref "$old@{upstream}" >/dev/null 2>&1; then
    git push origin :"$old" && git push --set-upstream origin "$new"
  fi
}

# Delete local branches already merged into the main/develop branch (never those two).
gbda() {
  local main dev branches
  main=$(git_main_branch); dev=$(git_develop_branch)
  branches=$(git branch --no-color --merged 2>/dev/null | command grep -vE "^[+*]" | command grep -vxE "[[:space:]]*(${main}|${dev})[[:space:]]*")
  [ -n "$branches" ] && echo "$branches" | command xargs git branch -d
}

# Clone a repository and cd into it.
gccd() {
  git clone --recurse-submodules "$@" || return
  local last
  for last in "$@"; do :; done
  cd "$(basename "${last%.git}")" 2>/dev/null || cd "$(basename "${last%/}")"
}

# Hard-reset and remove ALL untracked + ignored files (gpristine) / untracked only (gwipe).
gpristine() { git reset --hard && git clean -dffx; }
gwipe()     { git reset --hard && git clean -df; }
