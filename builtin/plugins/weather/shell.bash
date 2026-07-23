# Weather at the shell via wttr.in. `weather` uses your IP location; `weather <city>` a place.
# `forecast [city]` prints the full 3-day report.
weather()  { curl -fsS "https://wttr.in/${1// /+}?format=4" 2>/dev/null || echo "weather: offline?" >&2; }
forecast() { curl -fsS "https://wttr.in/${1// /+}" 2>/dev/null || echo "forecast: offline?" >&2; }
