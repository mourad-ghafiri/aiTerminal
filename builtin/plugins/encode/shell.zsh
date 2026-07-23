# Shell encoders/decoders — pure functions, no external dependencies.
b64()    { if [ $# -gt 0 ]; then printf '%s' "$*" | base64; else base64; fi; }
unb64()  { if [ $# -gt 0 ]; then printf '%s' "$*" | base64 -d; else base64 -d; fi; }
urlenc() { command -v python3 >/dev/null 2>&1 && python3 -c 'import sys,urllib.parse as u; print(u.quote(sys.argv[1]))' "$*"; }
urldec() { command -v python3 >/dev/null 2>&1 && python3 -c 'import sys,urllib.parse as u; print(u.unquote(sys.argv[1]))' "$*"; }
sha()    { if [ $# -gt 0 ] && [ -f "$1" ]; then shasum -a 256 "$1"; else printf '%s' "$*" | shasum -a 256 | cut -d' ' -f1; fi; }
sha1()   { if [ $# -gt 0 ] && [ -f "$1" ]; then shasum -a 1 "$1"; else printf '%s' "$*" | shasum -a 1 | cut -d' ' -f1; fi; }
sha512() { if [ $# -gt 0 ] && [ -f "$1" ]; then shasum -a 512 "$1"; else printf '%s' "$*" | shasum -a 512 | cut -d' ' -f1; fi; }
hexenc() { if [ $# -gt 0 ]; then printf '%s' "$*" | xxd -p | tr -d '\n'; echo; else xxd -p | tr -d '\n'; echo; fi; }
hexdec() { if [ $# -gt 0 ]; then printf '%s' "$*" | xxd -r -p; else xxd -r -p; fi; }
rot13()  { if [ $# -gt 0 ]; then printf '%s' "$*" | tr 'A-Za-z' 'N-ZA-Mn-za-m'; else tr 'A-Za-z' 'N-ZA-Mn-za-m'; fi; }
jsonpp() { if [ $# -gt 0 ] && [ -f "$1" ]; then python3 -m json.tool "$1"; else python3 -m json.tool; fi; }
uuid()   { uuidgen | tr 'A-F' 'a-f'; }
