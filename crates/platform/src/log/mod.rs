//! The diagnostic logger — leveled, async, file-backed.
//!
//! A from-scratch logger (no `log`/`tracing` crate) living in `platform` so every layer
//! above it (`framework`, `app`) can emit, while `corelib` stays pure. Design:
//!
//! - **Levels** ([`Level`]) with a lock-free [`AtomicU8`](std::sync::atomic::AtomicU8)
//!   threshold (default [`Level::Error`]); a disabled level costs one atomic load and
//!   never formats its message.
//! - **Non-blocking**: producers `try_send` a [`Record`] over a *bounded* channel to ONE
//!   background writer thread, so the UI/render thread never blocks on disk. A full buffer
//!   drops the record and bumps a counter (surfaced as a single warn line) rather than
//!   stalling.
//! - **Pluggable sink** ([`Sink`], the Strategy pattern). The default [`RotatingFileSink`]
//!   writes **one file per day** (`logs/YYYY-MM-DD.log`) and prunes files older than the
//!   retention window — bounded disk, never one ever-growing file.
//!
//! Usage (from any crate that depends on `platform`):
//! ```ignore
//! platform::log::init(logs_dir, Level::Error, 7);
//! platform::error!("disk write failed: {e}");
//! platform::info!("started {}", name);
//! ```

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Severity. The discriminant doubles as the ordering: a record passes when its level is
/// `<=` the active threshold. [`Off`](Level::Off) is a *threshold only* (silences all);
/// records are always `Error..=Trace`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub enum Level {
    Off = 0,
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

impl Level {
    /// Parse a config string (case-insensitive); unknown → the safe default [`Level::Error`].
    pub fn parse(s: &str) -> Level {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "silent" => Level::Off,
            "error" => Level::Error,
            "warn" | "warning" => Level::Warn,
            "info" => Level::Info,
            "debug" => Level::Debug,
            "trace" => Level::Trace,
            _ => Level::Error,
        }
    }

    /// The uppercase tag written into each log line.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Off => "OFF",
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        }
    }
}

/// One log entry — pure data handed to a [`Sink`].
pub struct Record {
    pub level: Level,
    pub unix_secs: i64,
    pub millis: u32,
    /// The emitting module (from `module_path!()` at the call site).
    pub target: &'static str,
    pub msg: String,
}

impl Record {
    /// The single canonical line format: `YYYY-MM-DD HH:MM:SS.mmm LEVEL target: message`.
    pub fn render(&self, offset_secs: i64) -> String {
        let ts = corelib::datetime::format(self.unix_secs, "%Y-%m-%d %H:%M:%S", offset_secs);
        format!("{ts}.{:03} {} {}: {}\n", self.millis, self.level.as_str(), self.target, self.msg)
    }

    /// The local calendar day (`YYYY-MM-DD`) this record belongs to — the rotation key.
    pub fn local_day(&self, offset_secs: i64) -> String {
        corelib::datetime::format(self.unix_secs, "%Y-%m-%d", offset_secs)
    }
}

/// A log destination (Strategy). The default is [`RotatingFileSink`]; the trait keeps the
/// writer thread agnostic so an alternative sink can be dropped in without touching it.
pub trait Sink: Send {
    fn write(&mut self, rec: &Record);
    fn flush(&mut self);
}

/// Writes one append-only file per local day (`<dir>/YYYY-MM-DD.log`), buffered, and prunes
/// files older than `retention_days` on construction and on each day rollover — so the log
/// folder stays bounded and is never a single ever-growing file.
pub struct RotatingFileSink {
    dir: PathBuf,
    retention_days: usize,
    offset_secs: i64,
    day: String,
    writer: Option<BufWriter<File>>,
}

impl RotatingFileSink {
    pub fn new(dir: PathBuf, retention_days: usize, offset_secs: i64) -> RotatingFileSink {
        let _ = std::fs::create_dir_all(&dir);
        let s = RotatingFileSink { dir, retention_days, offset_secs, day: String::new(), writer: None };
        s.prune();
        s
    }

    /// Open (append) the file for `day`, replacing any current handle.
    fn open_day(&mut self, day: &str) {
        let path = self.dir.join(format!("{day}.log"));
        if let Ok(f) = OpenOptions::new().create(true).append(true).open(&path) {
            self.writer = Some(BufWriter::new(f));
            self.day = day.to_string();
        } else {
            self.writer = None;
        }
    }

    /// Delete `*.log` files whose `YYYY-MM-DD` stem is older than the retention window.
    /// `retention_days == 0` keeps everything (pruning disabled).
    fn prune(&self) {
        if self.retention_days == 0 {
            return;
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let cutoff = now - (self.retention_days as i64) * 86_400;
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return };
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("log") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            // Day-stamped files only (`YYYY-MM-DD`); anything else is left alone.
            if let Some(secs) = corelib::datetime::parse(stem, Some("%Y-%m-%d"), self.offset_secs) {
                if secs < cutoff {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
}

impl Sink for RotatingFileSink {
    fn write(&mut self, rec: &Record) {
        let day = rec.local_day(self.offset_secs);
        if self.writer.is_none() || day != self.day {
            self.open_day(&day);
            self.prune(); // a rollover is the natural moment to drop stale days
        }
        if let Some(w) = self.writer.as_mut() {
            let _ = w.write_all(rec.render(self.offset_secs).as_bytes());
        }
    }

    fn flush(&mut self) {
        if let Some(w) = self.writer.as_mut() {
            let _ = w.flush();
        }
    }
}

/// The message the producers send to the writer thread.
enum Msg {
    Record(Record),
    /// Flush request with an ack channel — lets [`flush`] block until the buffer is on disk.
    Flush(SyncSender<()>),
}

/// The active threshold (default [`Level::Error`]). Lock-free so `enabled()` is a single load.
static LEVEL: AtomicU8 = AtomicU8::new(Level::Error as u8);
/// The channel to the writer thread, installed once by [`init`].
static SENDER: OnceLock<SyncSender<Msg>> = OnceLock::new();
/// Records dropped because the bounded buffer was full (surfaced as a warn line).
static DROPPED: AtomicU64 = AtomicU64::new(0);

/// The bounded buffer size — generous for the default low-volume (error) case, yet a hard
/// cap so a logging storm can never grow memory without bound.
const CHANNEL_CAPACITY: usize = 4096;

/// Whether a record at `level` would be emitted (a cheap atomic load — call it before any
/// formatting work). The macros use this to make a disabled level free.
pub fn enabled(level: Level) -> bool {
    (level as u8) <= LEVEL.load(Ordering::Relaxed)
}

/// Change the active threshold at runtime (e.g. from a settings UI).
pub fn set_level(level: Level) {
    LEVEL.store(level as u8, Ordering::Relaxed);
}

/// Install the logger: set the threshold and spawn the single background writer thread that
/// owns a [`RotatingFileSink`] over `dir`. Idempotent — a second call only updates the level
/// (the writer thread and channel are created once).
pub fn init(dir: PathBuf, level: Level, retention_days: usize) {
    set_level(level);
    let _ = SENDER.get_or_init(|| {
        let (tx, rx) = sync_channel::<Msg>(CHANNEL_CAPACITY);
        let sink = RotatingFileSink::new(dir, retention_days, crate::os::utc_offset_secs());
        std::thread::Builder::new()
            .name("tt-logger".into())
            .spawn(move || writer_loop(rx, Box::new(sink)))
            .ok();
        tx
    });
}

/// The writer thread: block for a record, drain the rest of the batch, write, then flush once
/// (batched I/O). A `Flush` message forces a flush and acks the caller.
fn writer_loop(rx: Receiver<Msg>, mut sink: Box<dyn Sink>) {
    while let Ok(first) = rx.recv() {
        let mut acks: Vec<SyncSender<()>> = Vec::new();
        let handle = |msg: Msg, sink: &mut Box<dyn Sink>, acks: &mut Vec<SyncSender<()>>| match msg {
            Msg::Record(r) => sink.write(&r),
            Msg::Flush(ack) => acks.push(ack),
        };
        handle(first, &mut sink, &mut acks);
        while let Ok(msg) = rx.try_recv() {
            handle(msg, &mut sink, &mut acks);
        }
        // Surface any records dropped under back-pressure as a single synthetic line.
        let dropped = DROPPED.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            sink.write(&make_record(Level::Warn, "platform::log", format!("dropped {dropped} log record(s): buffer full")));
        }
        sink.flush();
        for ack in acks {
            let _ = ack.send(());
        }
    }
}

/// Block until every buffered record is flushed to disk (best-effort — a no-op before
/// [`init`]). Used at process exit and in the panic hook so the last lines aren't lost.
pub fn flush() {
    if let Some(tx) = SENDER.get() {
        let (ack_tx, ack_rx) = sync_channel::<()>(1);
        if tx.try_send(Msg::Flush(ack_tx)).is_ok() {
            let _ = ack_rx.recv();
        }
    }
}

/// Build a [`Record`] stamped with the current wall-clock time (secs + millis).
fn make_record(level: Level, target: &'static str, msg: String) -> Record {
    let (unix_secs, millis) = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_millis()),
        Err(_) => (0, 0),
    };
    Record { level, unix_secs, millis, target, msg }
}

/// The macro back-end: build a record and hand it to the writer (non-blocking). Before
/// [`init`], `Error`/`Warn` fall back to stderr so early-boot failures are never silent.
#[doc(hidden)]
pub fn __emit(level: Level, target: &'static str, args: std::fmt::Arguments) {
    let msg = args.to_string();
    match SENDER.get() {
        Some(tx) => {
            if tx.try_send(Msg::Record(make_record(level, target, msg))).is_err() {
                DROPPED.fetch_add(1, Ordering::Relaxed);
            }
        }
        None => {
            if level <= Level::Warn {
                eprintln!("{} {target}: {msg}", level.as_str());
            }
        }
    }
}

/// Log at `Error`. `platform::error!("...", args)` — formats only if the level is enabled.
#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => {{
        if $crate::log::enabled($crate::log::Level::Error) {
            $crate::log::__emit($crate::log::Level::Error, module_path!(), format_args!($($arg)*));
        }
    }};
}

/// Log at `Warn`.
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {{
        if $crate::log::enabled($crate::log::Level::Warn) {
            $crate::log::__emit($crate::log::Level::Warn, module_path!(), format_args!($($arg)*));
        }
    }};
}

/// Log at `Info`.
#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => {{
        if $crate::log::enabled($crate::log::Level::Info) {
            $crate::log::__emit($crate::log::Level::Info, module_path!(), format_args!($($arg)*));
        }
    }};
}

/// Log at `Debug`.
#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => {{
        if $crate::log::enabled($crate::log::Level::Debug) {
            $crate::log::__emit($crate::log::Level::Debug, module_path!(), format_args!($($arg)*));
        }
    }};
}

/// Log at `Trace`.
#[macro_export]
macro_rules! trace {
    ($($arg:tt)*) => {{
        if $crate::log::enabled($crate::log::Level::Trace) {
            $crate::log::__emit($crate::log::Level::Trace, module_path!(), format_args!($($arg)*));
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tt-log-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn rec(level: Level, unix_secs: i64, msg: &str) -> Record {
        Record { level, unix_secs, millis: 7, target: "t::m", msg: msg.into() }
    }

    #[test]
    fn level_parses_orders_and_thresholds() {
        assert_eq!(Level::parse("ERROR"), Level::Error);
        assert_eq!(Level::parse("Warn"), Level::Warn);
        assert_eq!(Level::parse("off"), Level::Off);
        assert_eq!(Level::parse("nonsense"), Level::Error); // safe default
        assert!(Level::Error < Level::Warn && Level::Warn < Level::Info);
        assert_eq!(Level::Error.as_str(), "ERROR");
    }

    #[test]
    fn record_renders_the_canonical_line() {
        let secs = 1_782_604_800;
        let ts = corelib::datetime::format(secs, "%Y-%m-%d %H:%M:%S", 0);
        let line = rec(Level::Error, secs, "boom").render(0);
        assert_eq!(line, format!("{ts}.007 ERROR t::m: boom\n"));
    }

    #[test]
    fn sink_writes_today_and_rotates_per_day() {
        let dir = tmp("rotate");
        // Retention 0 (pruning off): the fixture days are FIXED timestamps, so any
        // real retention window would eventually prune them as the actual date
        // moves on (a time-bomb test). Pruning has its own test below.
        let mut sink = RotatingFileSink::new(dir.clone(), 0, 0);
        let day1 = 1_782_604_800;
        let day2 = day1 + 86_400; // the following day
        let d1 = corelib::datetime::format(day1, "%Y-%m-%d", 0);
        let d2 = corelib::datetime::format(day2, "%Y-%m-%d", 0);
        assert_ne!(d1, d2, "the two timestamps must fall on different days");
        sink.write(&rec(Level::Error, day1, "first"));
        sink.write(&rec(Level::Error, day1 + 5, "second"));
        sink.write(&rec(Level::Warn, day2, "next day"));
        sink.flush();
        let a = std::fs::read_to_string(dir.join(format!("{d1}.log"))).unwrap();
        let b = std::fs::read_to_string(dir.join(format!("{d2}.log"))).unwrap();
        assert!(a.contains("first") && a.contains("second") && !a.contains("next day"));
        assert!(b.contains("next day") && !b.contains("first"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_removes_files_older_than_retention() {
        let dir = tmp("prune");
        std::fs::create_dir_all(&dir).unwrap();
        // An ancient day file + a recent one (today).
        std::fs::write(dir.join("2000-01-01.log"), "old\n").unwrap();
        let today = corelib::datetime::format(
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64,
            "%Y-%m-%d",
            0,
        );
        std::fs::write(dir.join(format!("{today}.log")), "new\n").unwrap();
        // Construction prunes with a 7-day window.
        let _sink = RotatingFileSink::new(dir.clone(), 7, 0);
        assert!(!dir.join("2000-01-01.log").exists(), "ancient file pruned");
        assert!(dir.join(format!("{today}.log")).exists(), "today's file kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn end_to_end_init_emit_flush_filters_by_level() {
        let dir = tmp("e2e");
        // Default threshold is Error; init with Error so info! is filtered out.
        init(dir.clone(), Level::Error, 7);
        assert!(enabled(Level::Error) && !enabled(Level::Info));
        crate::error!("written {}", 1);
        crate::info!("filtered {}", 2);
        flush();
        let today = corelib::datetime::format(
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64,
            "%Y-%m-%d",
            crate::os::utc_offset_secs(),
        );
        let body = std::fs::read_to_string(dir.join(format!("{today}.log"))).unwrap();
        assert!(body.contains("written 1"), "error line present: {body:?}");
        assert!(!body.contains("filtered"), "info filtered at error threshold");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
