# Logging

A from-scratch, leveled diagnostic logger (`platform::log`) — async (a background
writer thread; never blocks the render loop), one file per day, auto-pruned.

- Files: `~/.aiTerminal/logs/YYYY-MM-DD.log`
- Config: `[logging] level = "off|error|warn|info|debug|trace"` (default `error`),
  `retention_days` (default 7; `0` keeps all).
- Panics are additionally appended to `~/.aiTerminal/crash.log` with thread +
  location, and the event loop drops the frame instead of aborting — so a crash is
  diagnosable *and* survivable.

Only the interactive window initializes the logger; the CLI stays silent on disk.
