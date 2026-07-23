# Open a web search in the browser. Spaces in the query become '+', which every engine
# accepts; the opener is chosen per platform (open / xdg-open).
__tt_open() { if command -v open >/dev/null 2>&1; then open "$1"; elif command -v xdg-open >/dev/null 2>&1; then xdg-open "$1"; else print -r -- "$1"; fi; }
__tt_search() { local base=$1; shift; __tt_open "$base${*// /+}"; }
google() { __tt_search "https://www.google.com/search?q=" "$@"; }
ddg()    { __tt_search "https://duckduckgo.com/?q=" "$@"; }
so()     { __tt_search "https://stackoverflow.com/search?q=" "$@"; }
ghs()    { __tt_search "https://github.com/search?q=" "$@"; }
yt()     { __tt_search "https://www.youtube.com/results?search_query=" "$@"; }
wiki()   { __tt_search "https://en.wikipedia.org/w/index.php?search=" "$@"; }
npmjs()  { __tt_search "https://www.npmjs.com/search?q=" "$@"; }
mdn()    { __tt_search "https://developer.mozilla.org/en-US/search?q=" "$@"; }
brave()    { __tt_search "https://search.brave.com/search?q=" "$@"; }
bing()     { __tt_search "https://www.bing.com/search?q=" "$@"; }
reddit()   { __tt_search "https://www.reddit.com/search/?q=" "$@"; }
gimages()  { __tt_search "https://www.google.com/search?tbm=isch&q=" "$@"; }
maps()     { __tt_search "https://www.google.com/maps/search/" "$@"; }
translate(){ __tt_search "https://translate.google.com/?sl=auto&tl=en&text=" "$@"; }
crates()   { __tt_search "https://crates.io/search?q=" "$@"; }
pypi()     { __tt_search "https://pypi.org/search/?q=" "$@"; }
archwiki() { __tt_search "https://wiki.archlinux.org/index.php?search=" "$@"; }
hub()      { __tt_search "https://github.com/search?type=code&q=" "$@"; }
unsplash() { __tt_search "https://unsplash.com/s/photos/" "$@"; }
