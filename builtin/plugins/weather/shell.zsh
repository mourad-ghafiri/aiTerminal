# Weather at the shell via wttr.in. `weather` uses your IP location; `weather <city>` a place.
# `forecast [city]` prints the full 3-day report.
weather()  { curl -fsS "https://wttr.in/${1// /+}?format=4" 2>/dev/null || print -u2 "weather: offline?"; }
forecast() { curl -fsS "https://wttr.in/${1// /+}" 2>/dev/null || print -u2 "forecast: offline?"; }
