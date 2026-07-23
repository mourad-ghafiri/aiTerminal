# Enable bash-completion if it's installed (Homebrew or system).
for f in /opt/homebrew/etc/profile.d/bash_completion.sh /usr/local/etc/profile.d/bash_completion.sh /etc/bash_completion; do
  [ -r "$f" ] && source "$f" && break
done
