# `x` extracts almost any archive by extension; `pack` creates one. Portable (identical in shell.bash).
#
#   x [-r|--remove] [-t|--to <dir>] <archive>...   extract (optionally into <dir>, deleting on success)
#   pack <archive.{tar.gz,tar.bz2,tar.xz,tar.zst,tar,zip,7z}> <files>...   create an archive

x() {
  local remove=0 dest="" f
  while [ "$#" -gt 0 ]; do
    case "$1" in
      -r|--remove) remove=1; shift ;;
      -t|--to)     dest="$2"; shift 2 ;;
      --)          shift; break ;;
      -*)          echo "x: unknown option: $1" >&2; return 1 ;;
      *)           break ;;
    esac
  done
  [ "$#" -gt 0 ] || { echo "usage: x [-r] [-t <dir>] <archive>..." >&2; return 1; }
  for f in "$@"; do
    [ -f "$f" ] || { echo "x: no such file: $f" >&2; continue; }
    local abs out
    abs="$(cd "$(dirname "$f")" && pwd)/$(basename "$f")"
    out="."
    [ -n "$dest" ] && { mkdir -p "$dest"; out="$dest"; }
    if ( cd "$out" 2>/dev/null || exit 1
      case "$f" in
        *.tar.bz2|*.tbz|*.tbz2)  tar xjf "$abs" ;;
        *.tar.gz|*.tgz)          tar xzf "$abs" ;;
        *.tar.xz|*.txz)          tar xJf "$abs" ;;
        *.tar.zst|*.tzst)        tar --zstd -xf "$abs" 2>/dev/null || { zstd -dc "$abs" | tar xf -; } ;;
        *.tar.lz4)               lz4 -dc "$abs" | tar xf - ;;
        *.tar.lzma|*.tlz)        tar --lzma -xf "$abs" 2>/dev/null || { lzma -dc "$abs" | tar xf -; } ;;
        *.tar.lz|*.tar)          tar xf "$abs" ;;
        *.gz)                    gunzip -k "$abs" ;;
        *.bz2)                   bunzip2 -k "$abs" ;;
        *.xz)                    unxz -k "$abs" ;;
        *.lzma)                  unlzma -k "$abs" ;;
        *.zst)                   zstd -dk "$abs" ;;
        *.lz4)                   lz4 -dk "$abs" ;;
        *.Z)                     uncompress "$abs" ;;
        *.zip|*.jar|*.war|*.ear|*.whl|*.apk|*.aar|*.vsix|*.ipa|*.xpi|*.crx|*.sublime-package) unzip -q "$abs" ;;
        *.rar)                   unrar x -ad "$abs" 2>/dev/null || unar -o . "$abs" ;;
        *.7z|*.pk7)              7z x "$abs" ;;
        *.deb)                   ar x "$abs" ;;
        *.rpm)                   rpm2cpio "$abs" | cpio -idmv ;;
        *.cpio)                  cpio -idmv < "$abs" ;;
        *.cab|*.exe)             cabextract "$abs" ;;
        *.lrz)                   lrunzip "$abs" ;;
        *) echo "x: don't know how to extract '$f'" >&2; exit 2 ;;
      esac
    ); then
      [ "$remove" -eq 1 ] && rm -f "$abs"
    fi
  done
}

pack() {
  local a="$1"; shift 2>/dev/null
  [ -n "$a" ] && [ "$#" -gt 0 ] || { echo "usage: pack <archive.{tar.gz,tar.bz2,tar.xz,tar.zst,tar,zip,7z}> <files>..." >&2; return 1; }
  case "$a" in
    *.tar.gz|*.tgz)    tar czf "$a" "$@" ;;
    *.tar.bz2|*.tbz2)  tar cjf "$a" "$@" ;;
    *.tar.xz|*.txz)    tar cJf "$a" "$@" ;;
    *.tar.zst|*.tzst)  tar --zstd -cf "$a" "$@" 2>/dev/null || { tar cf - "$@" | zstd -o "$a"; } ;;
    *.tar)             tar cf  "$a" "$@" ;;
    *.zip)             zip -qr "$a" "$@" ;;
    *.7z)              7z a "$a" "$@" ;;
    *) echo "pack: unknown archive type '$a'" >&2; return 1 ;;
  esac && echo "packed $a"
}
