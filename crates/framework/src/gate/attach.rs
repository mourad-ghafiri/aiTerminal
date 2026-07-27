//! Noticing that a program has taken the terminal, and mirroring it to the chat.
//!
//! When you run Claude Code, Codex, `vim`, or a `psql` REPL, the shell is no longer the
//! thing you are talking to — the program is. The gate has to notice that, stop treating
//! your messages as shell commands, and start showing you the program's screen.
//!
//! **Nothing here knows about any specific program**, and nothing here ever should. Two
//! completely generic signals do the work:
//!
//! 1. **The terminal protocol.** A program that manages the whole screen says so, in
//!    DEC private modes — alt screen, bracketed paste, application cursor keys, mouse
//!    reporting. `Term::app_control()` is that fact. It deliberately does not require the
//!    alternate screen, because plenty of modern CLIs render inline.
//! 2. **The shape of a prompt.** A REPL sets no modes at all, so the fallback is the
//!    universal picture of one: a command is running, output has gone quiet, and the
//!    cursor is parked *after text on its own line* (`>>> `, `psql=#`). A command merely
//!    being slow leaves the cursor at column 0, so `sleep 60` does not trip it.
//!
//! Frames are debounced rather than streamed: a TUI repaints tens of times a second, and
//! a chat is not a video codec. We wait for the screen to settle, then send one frame.

/// What the terminal reports about who is driving it.
///
/// The policy below is the whole detector, and the reason it lives here rather than in
/// the emulator: `platform::term` reports *facts*, and only the gate knows what they
/// mean.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Signals {
    /// The alternate screen is up.
    pub alt: bool,
    /// Mouse reporting is on.
    pub mouse: bool,
    /// Bracketed paste is on.
    pub bracketed: bool,
    /// Application cursor keys (DECCKM) are on.
    pub app_cursor: bool,
    /// A command is actually executing — from the shell's own preexec/precmd marks.
    pub command_running: bool,
}

impl Signals {
    /// Is a *program* driving this terminal, as opposed to the shell?
    ///
    /// Getting this wrong breaks everything, and the trap is subtle: **a shell's line
    /// editor arms bracketed paste and application cursor keys at every prompt.** zsh
    /// reports `zle_bracketed_paste = ESC[?2004h` and `smkx = ESC[?1h ESC=`; bash ≥ 5.1
    /// does the same through readline. So neither means anything on its own — treating
    /// them as evidence attaches the gate to your own shell the moment it starts, and
    /// never lets go.
    ///
    /// Two signals *are* shell-proof, because no shell sets them: the alternate screen
    /// and mouse reporting. For the ambiguous pair, the shell integration already tells
    /// us the truth — a command runs between the `preexec` and `precmd` marks. At a
    /// prompt nothing is running, so the shell's own modes are correctly ignored; once
    /// `claude` starts, the very same modes become decisive.
    ///
    /// Without shell integration (fish, or `[shell] integration = false`) only
    /// alternate-screen and mouse programs are detectable — `/status` says so rather
    /// than pretending otherwise.
    pub fn owns_terminal(&self) -> bool {
        self.alt || self.mouse || ((self.bracketed || self.app_cursor) && self.command_running)
    }
}

/// Why we attached — shown to the user, and it decides how much we claim to know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// The program declared itself in the terminal protocol.
    AppControl,
    /// No modes set, but the cursor is sitting at what looks like a prompt.
    Prompt,
}

/// What the driver should do about the attachment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Attached(Why),
    /// The screen settled — render and send it.
    Frame,
    Detached,
}

/// The screen must be still this long before a frame is sent. Long enough to let a
/// repaint burst finish, short enough that answering a prompt feels immediate.
const SETTLE_MS: u64 = 600;
/// Never edit the live message more often than this. A program that streams output
/// (an AI agent writing a reply) would otherwise redraw continuously and earn a rate
/// limit for its trouble.
const MIN_FRAME_MS: u64 = 2_000;
/// A program must look gone this long before we detach — mode flags flicker between
/// frames in some apps, and flapping in and out of app mode would be maddening.
const DETACH_GRACE_MS: u64 = 900;
/// Output must be quiet this long before a cursor-at-a-prompt counts as waiting.
pub const PROMPT_QUIET_MS: u64 = 1_500;

/// Tracks whether a program owns the terminal, and when to ship a frame.
pub struct Attacher {
    on: Option<Why>,
    /// Generation of the mirror when we last sent a frame.
    framed_gen: u64,
    /// Generation at the previous observation — how we tell "the screen moved just
    /// now" from "it moved at some point since our last frame". Timing the settle
    /// against the framed generation instead would restart the clock on every call
    /// and no frame would ever be due.
    seen_gen: u64,
    /// When the screen last changed.
    changed_ms: u64,
    /// When we last sent a frame.
    framed_ms: u64,
    /// When the attach conditions stopped holding (for the grace period).
    lost_ms: Option<u64>,
}

impl Default for Attacher {
    fn default() -> Self {
        Self::new()
    }
}

impl Attacher {
    pub fn new() -> Self {
        Attacher { on: None, framed_gen: 0, seen_gen: 0, changed_ms: 0, framed_ms: 0, lost_ms: None }
    }

    pub fn attached(&self) -> bool {
        self.on.is_some()
    }

    pub fn why(&self) -> Option<Why> {
        self.on
    }

    /// Force a frame as soon as the screen settles — used after we send input.
    ///
    /// This resets the rate-limit clock too. That limit exists to stop a *program*
    /// flooding the chat while it streams; throttling a deliberate user action by the
    /// same two seconds just reads as "I tapped Yes and nothing happened".
    pub fn invalidate(&mut self) {
        self.framed_gen = self.framed_gen.wrapping_sub(1);
        self.framed_ms = 0;
    }

    /// Detach immediately (the command that owned the terminal ended).
    pub fn release(&mut self) -> Option<Event> {
        self.lost_ms = None;
        self.on.take().map(|_| Event::Detached)
    }

    /// Observe the mirror. `generation` is `Term::generation()`; `at_prompt` is the
    /// driver's REPL heuristic.
    pub fn observe(&mut self, app_control: bool, at_prompt: bool, generation: u64, now: u64) -> Option<Event> {
        if generation != self.seen_gen {
            // The screen moved on THIS observation; the settle window starts now.
            self.seen_gen = generation;
            self.changed_ms = now;
        }

        let want = if app_control {
            Some(Why::AppControl)
        } else if at_prompt {
            Some(Why::Prompt)
        } else {
            None
        };

        match (self.on, want) {
            // Nothing owns the terminal and nothing did — the ordinary shell case.
            (None, None) => {
                self.lost_ms = None;
                None
            }
            // A program just took over. Frame it right away: the user needs to see what
            // appeared, not wait for it to redraw.
            (None, Some(why)) => {
                self.on = Some(why);
                self.lost_ms = None;
                self.framed_gen = generation;
                self.framed_ms = now;
                Some(Event::Attached(why))
            }
            (Some(_), Some(why)) => {
                self.lost_ms = None;
                // A prompt-attached session that later declares itself is still the same
                // session; just remember the stronger reason.
                if why == Why::AppControl {
                    self.on = Some(Why::AppControl);
                }
                self.frame_due(generation, now).then(|| {
                    self.framed_gen = generation;
                    self.framed_ms = now;
                    Event::Frame
                })
            }
            // The conditions stopped holding — wait out the grace period before letting go.
            (Some(_), None) => {
                let since = *self.lost_ms.get_or_insert(now);
                if now.saturating_sub(since) >= DETACH_GRACE_MS {
                    self.on = None;
                    self.lost_ms = None;
                    return Some(Event::Detached);
                }
                None
            }
        }
    }

    fn frame_due(&self, generation: u64, now: u64) -> bool {
        generation != self.framed_gen
            && now.saturating_sub(self.changed_ms) >= SETTLE_MS
            && now.saturating_sub(self.framed_ms) >= MIN_FRAME_MS
    }
}

/// A choice the program is offering, as a button: `(label, callback data)`.
pub type Choice = (String, String);

/// Buttons for the choices visible on screen — read off the *shape* of the question,
/// not from any knowledge of the program asking it.
///
/// Two shapes cover essentially every terminal prompt ever written: a numbered list
/// (`❯ 1. Yes`, `2) No`) and a yes/no bracket (`[y/N]`, `(y/n)`). Claude Code, `apt`,
/// `git add -p` and a hand-rolled `read -p` all fall out of the same two patterns.
pub fn choices(screen: &[String]) -> Vec<Choice> {
    // Only the tail of the screen matters: a question is the last thing printed, and an
    // earlier numbered list (a search result, a changelog) is not something to answer.
    let tail = screen.len().saturating_sub(14);
    let recent = &screen[tail..];
    let mut out: Vec<Choice> = Vec::new();

    // A numbered list is only something to ANSWER when it is a question. An agent
    // listing its plan, a test runner listing failures, and a numbered changelog all
    // look identical otherwise — and turning those into buttons sends stray digits
    // into the program the moment someone taps one.
    if !asks_something(recent) {
        return yes_no_choices(recent);
    }
    for line in recent {
        if let Some((n, label)) = numbered(line) {
            if !out.iter().any(|(_, d)| d == &format!("k:{n}")) {
                out.push((format!("{n} · {}", clip(label, 18)), format!("k:{n}")));
            }
        }
    }
    if out.len() > 6 {
        out.truncate(6);
    }
    if !out.is_empty() {
        return out;
    }

    yes_no_choices(recent)
}

/// Is the program actually asking something?
///
/// Either a line ends in a question mark or a colon (every prompt ever written), or one
/// of the options carries a selection marker — which only appears on a live menu.
fn asks_something(recent: &[String]) -> bool {
    recent.iter().any(|l| {
        let t = l.trim_end();
        t.ends_with('?') || t.ends_with(':') || {
            let s = t.trim_start();
            s.starts_with(['❯', '▶', '→']) && numbered(l).is_some()
        }
    })
}

/// A yes/no bracket, honouring which side is capitalized (the default) by putting it
/// first. Only the last few lines: an already-answered `[y/N]` still on screen must not
/// keep offering buttons under a different question.
fn yes_no_choices(recent: &[String]) -> Vec<Choice> {
    let from = recent.len().saturating_sub(4);
    for line in recent[from..].iter().rev() {
        if let Some(default_yes) = yes_no(line) {
            return if default_yes {
                vec![("Y · yes".into(), "k:Y".into()), ("n · no".into(), "k:n".into())]
            } else {
                vec![("y · yes".into(), "k:y".into()), ("N · no".into(), "k:N".into())]
            };
        }
    }
    Vec::new()
}

/// `❯ 1. Yes, and don't ask again` → `(1, "Yes, and don't ask again")`.
fn numbered(line: &str) -> Option<(u8, &str)> {
    let t = line.trim_start();
    // An optional selection marker, then the digit.
    let t = t.strip_prefix(['❯', '>', '▶', '•', '*', '→', '│', '┃']).map(str::trim_start).unwrap_or(t);
    let mut it = t.char_indices();
    let (_, first) = it.next()?;
    let n = first.to_digit(10)? as u8;
    if n == 0 {
        return None;
    }
    let (i, sep) = it.next()?;
    if sep != '.' && sep != ')' {
        return None;
    }
    let rest = t[i + sep.len_utf8()..].trim_start();
    // A bare `1.` with nothing after it is prose (a version, a list marker), not a choice.
    (!rest.is_empty()).then_some((n, rest))
}

/// `Overwrite? [y/N]` → `Some(false)`; `Continue (Y/n)` → `Some(true)`.
fn yes_no(line: &str) -> Option<bool> {
    let mut chars: Vec<char> = Vec::new();
    for c in line.chars() {
        chars.push(c);
    }
    for w in chars.windows(5) {
        let open = w[0];
        let close = w[4];
        let bracketed = (open == '[' && close == ']') || (open == '(' && close == ')');
        if bracketed && w[2] == '/' {
            let (a, b) = (w[1], w[3]);
            if a.eq_ignore_ascii_case(&'y') && b.eq_ignore_ascii_case(&'n') {
                return Some(a.is_uppercase());
            }
            if a.eq_ignore_ascii_case(&'n') && b.eq_ignore_ascii_case(&'y') {
                return Some(b.is_uppercase());
            }
        }
    }
    None
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>().trim_end().to_string() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the attacher with a fixed generation, i.e. "the screen is not changing".
    fn still(a: &mut Attacher, app: bool, gen: u64, now: u64) -> Option<Event> {
        a.observe(app, false, gen, now)
    }

    #[test]
    fn a_shell_prompt_is_not_a_program() {
        // zsh and bash arm BOTH ambiguous modes at every prompt. If either counted on
        // its own, the gate would attach to your own shell and never let go.
        let prompt = Signals { bracketed: true, app_cursor: true, ..Default::default() };
        assert!(!prompt.owns_terminal());
    }

    #[test]
    fn the_same_modes_are_decisive_once_a_command_is_running() {
        let running = Signals { bracketed: true, command_running: true, ..Default::default() };
        assert!(running.owns_terminal(), "an inline CLI never touches the alternate screen");
        let cursor = Signals { app_cursor: true, command_running: true, ..Default::default() };
        assert!(cursor.owns_terminal());
    }

    #[test]
    fn the_shell_proof_signals_stand_on_their_own() {
        // No shell sets these, so they work even with no shell integration at all —
        // which is what keeps fish and `[shell] integration = false` usable.
        assert!(Signals { alt: true, ..Default::default() }.owns_terminal());
        assert!(Signals { mouse: true, ..Default::default() }.owns_terminal());
    }

    #[test]
    fn a_quiet_terminal_owns_nothing() {
        assert!(!Signals::default().owns_terminal());
        assert!(!Signals { command_running: true, ..Default::default() }.owns_terminal());
    }

    #[test]
    fn a_program_taking_the_terminal_attaches_at_once_and_frames_immediately() {
        // No command needs to be running: an app started at the local keyboard must be
        // drivable from the phone too.
        let mut a = Attacher::new();
        assert_eq!(still(&mut a, false, 1, 0), None);
        assert_eq!(still(&mut a, true, 2, 100), Some(Event::Attached(Why::AppControl)));
        assert!(a.attached());
    }

    #[test]
    fn a_repl_prompt_attaches_but_a_merely_slow_command_does_not() {
        let mut a = Attacher::new();
        // `sleep 60`: quiet, but the cursor is at column 0 — the driver reports no prompt.
        assert_eq!(a.observe(false, false, 5, 1_000), None);
        assert!(!a.attached());
        // `python3`: quiet, cursor parked after `>>> `.
        assert_eq!(a.observe(false, true, 5, 2_000), Some(Event::Attached(Why::Prompt)));
    }

    #[test]
    fn frames_wait_for_the_screen_to_settle() {
        let mut a = Attacher::new();
        a.observe(true, false, 1, 0);
        // Repainting: the generation keeps moving, so nothing is sent.
        let mut t = 0;
        for g in 2..12 {
            t += 200;
            assert_eq!(a.observe(true, false, g, t), None, "must not frame mid-repaint");
        }
        // It stops repainting: the generation now holds still at its last value, and the
        // settle window runs from that last change (t), not from each observation.
        let g = 11;
        assert_eq!(a.observe(true, false, g, t + SETTLE_MS - 1), None);
        assert_eq!(a.observe(true, false, g, t + SETTLE_MS), Some(Event::Frame));
        // And not again while nothing changes.
        assert_eq!(a.observe(true, false, g, t + SETTLE_MS + 10_000), None);
    }

    #[test]
    fn a_streaming_program_cannot_outrun_the_rate_limit() {
        // An AI agent writing a long reply settles briefly between chunks; editing the
        // live message on every one of those would earn a 429.
        let mut a = Attacher::new();
        a.observe(true, false, 1, 0);
        let mut sent = 0;
        let mut g = 1;
        for step in 1..=40 {
            let now = step * 700; // a change, then a settle, over and over
            g += 1;
            a.observe(true, false, g, now);
            if a.observe(true, false, g, now + SETTLE_MS).is_some() {
                sent += 1;
            }
        }
        assert!(sent > 0, "some frames must get through");
        let span_ms = 40 * 700 + SETTLE_MS;
        assert!(sent <= span_ms / MIN_FRAME_MS + 1, "sent {sent} frames in {span_ms}ms — too fast");
    }

    #[test]
    fn a_brief_flicker_in_the_modes_does_not_flap() {
        let mut a = Attacher::new();
        a.observe(true, false, 1, 0);
        // Modes momentarily read false between frames.
        assert_eq!(a.observe(false, false, 2, 100), None);
        assert!(a.attached(), "still attached during the grace period");
        assert_eq!(a.observe(true, false, 3, 300), None);
        assert!(a.attached());
    }

    #[test]
    fn detaching_happens_once_the_program_is_really_gone() {
        let mut a = Attacher::new();
        a.observe(true, false, 1, 0);
        assert_eq!(a.observe(false, false, 2, 100), None);
        assert_eq!(a.observe(false, false, 2, 100 + DETACH_GRACE_MS), Some(Event::Detached));
        assert!(!a.attached());
        assert_eq!(a.observe(false, false, 2, 99_999), None, "and stays detached quietly");
    }

    #[test]
    fn a_finished_command_releases_the_attachment_immediately() {
        let mut a = Attacher::new();
        a.observe(false, true, 1, 0);
        assert_eq!(a.release(), Some(Event::Detached));
        assert!(!a.attached());
        assert_eq!(a.release(), None, "releasing twice is not an event");
    }

    #[test]
    fn a_prompt_session_upgrades_when_the_program_declares_itself() {
        let mut a = Attacher::new();
        a.observe(false, true, 1, 0);
        assert_eq!(a.why(), Some(Why::Prompt));
        a.observe(true, false, 1, 100);
        assert_eq!(a.why(), Some(Why::AppControl), "no re-attach, just a better reason");
    }

    #[test]
    fn invalidating_forces_the_next_settled_frame() {
        // After we type into the program, the user should see the result even if the
        // generation happens to land where our last frame did.
        let mut a = Attacher::new();
        a.observe(true, false, 7, 0);
        assert_eq!(a.observe(true, false, 7, 10_000), None, "nothing changed");
        a.invalidate();
        assert_eq!(a.observe(true, false, 7, 20_000), Some(Event::Frame));
    }

    // ── choices ──────────────────────────────────────────────────────────────

    #[test]
    fn a_numbered_question_becomes_buttons() {
        // The shape every coding agent uses for a permission prompt.
        let screen: Vec<String> = [
            "  Edit src/parse.rs",
            "",
            "Do you want to make this edit?",
            "❯ 1. Yes",
            "  2. Yes, and don't ask again this session",
            "  3. No, and tell Claude what to do differently",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let c = choices(&screen);
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].1, "k:1");
        assert_eq!(c[1].1, "k:2");
        assert_eq!(c[2].1, "k:3");
        assert!(c[0].0.starts_with("1 · Yes"));
        assert!(c[1].0.chars().count() <= 24, "labels stay button-sized: {:?}", c[1].0);
    }

    #[test]
    fn a_plan_or_a_failure_list_is_not_a_question() {
        // The misfire that matters: tapping one of these would send a digit into the
        // program for no reason.
        let plan: Vec<String> = ["Here is what I will do", "  1. Read the file", "  2. Apply the edit", "Working"]
            .iter().map(|s| s.to_string()).collect();
        assert!(choices(&plan).is_empty(), "{:?}", choices(&plan));

        let failures: Vec<String> = ["Failures", "  1) MyClass does something", "  2) MyClass does another"]
            .iter().map(|s| s.to_string()).collect();
        assert!(choices(&failures).is_empty(), "{:?}", choices(&failures));
    }

    #[test]
    fn a_stale_yes_no_further_up_the_screen_is_not_offered() {
        let mut screen: Vec<String> = vec!["Overwrite? [y/N]".into(), "n".into()];
        screen.extend((0..6).map(|i| format!("copied file {i}")));
        assert!(choices(&screen).is_empty(), "an answered prompt must stop offering buttons");
    }

    #[test]
    fn other_numbering_styles_work_too() {
        let screen: Vec<String> = ["Pick a target:", " 1) debug", " 2) release"].iter().map(|s| s.to_string()).collect();
        let c = choices(&screen);
        assert_eq!(c.iter().map(|(_, d)| d.as_str()).collect::<Vec<_>>(), ["k:1", "k:2"]);
    }

    #[test]
    fn a_yes_no_bracket_becomes_two_buttons_with_the_default_first() {
        let no_default: Vec<String> = vec!["Overwrite existing file? [y/N]".into()];
        assert_eq!(choices(&no_default)[0].1, "k:y");
        assert_eq!(choices(&no_default)[1].1, "k:N");

        let yes_default: Vec<String> = vec!["Continue (Y/n)".into()];
        assert_eq!(choices(&yes_default)[0].1, "k:Y");
        assert_eq!(choices(&yes_default)[1].1, "k:n");
    }

    #[test]
    fn ordinary_output_offers_no_buttons() {
        // The failure that would matter: turning a changelog or a search result into
        // buttons that silently send keystrokes.
        let screen: Vec<String> = [
            "aiTerminal v1.2.0",
            "Rust 1.83.0 (aarch64-apple-darwin)",
            "  see docs/gate.md for details",
            "$ ",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert!(choices(&screen).is_empty(), "{:?}", choices(&screen));
    }

    #[test]
    fn a_bare_number_is_not_a_choice() {
        let screen: Vec<String> = vec!["1.".into(), "2)".into(), "  3.   ".into()];
        assert!(choices(&screen).is_empty());
    }

    #[test]
    fn only_the_tail_of_the_screen_is_read() {
        // A numbered list scrolled far above the current question must not become the
        // answer buttons.
        let mut screen: Vec<String> = vec!["  1. an old menu item".into(), "  2. another".into()];
        screen.extend((0..30).map(|i| format!("output line {i}")));
        assert!(choices(&screen).is_empty());
    }

    #[test]
    fn at_most_six_buttons_are_offered() {
        let mut screen: Vec<String> = vec!["Which one?".to_string()];
        screen.extend((1..=9).map(|i| format!("  {i}. option {i}")));
        assert_eq!(choices(&screen).len(), 6);
    }
}
