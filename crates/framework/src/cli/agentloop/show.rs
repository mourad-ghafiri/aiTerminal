use crate::cli::style::{muted, reset};

/// `@loop` — the recent runs, newest first.
pub(crate) fn loop_list() -> i32 {
    crate::config::Config::ensure_default();
    let cfg = crate::config::Config::load();
    crate::i18n::install(cfg.i18n_catalog());
    let runs = crate::loops::list();
    if runs.is_empty() {
        println!("{}", crate::i18n::translate("loop.none", &[]));
        return 0;
    }
    let now = crate::loops::now();
    let (dim, r) = (muted(), reset());
    println!("{}", crate::i18n::translate("loop.header", &[runs.len().to_string()]));
    for run in runs {
        let goal = clip_tail(&run.goal, 46);
        let age = crate::loops::human_age(now.saturating_sub(run.finished.unwrap_or(run.started)));
        println!("  {} {} {:<9} {goal}  {dim}({age} ago){r}", run.status_glyph(), run.id, run.status);
        let p = &run.progress;
        let iters = format!("{}/{} iteration(s)", p.iterations, run.bounds.max);
        println!("      {dim}{} \u{b7} {iters}{r}", run.verifier.describe());
    }
    println!("\n{}", crate::i18n::translate("loop.run_hint", &[]));
    0
}

/// One run's full record.
pub(crate) fn loop_show(id: &str) -> i32 {
    let id = match crate::loops::resolve(id) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("aiTerminal: {e}");
            return 2;
        }
    };
    let Some(run) = crate::loops::read(&id) else { return 2 };
    let (dim, r) = (muted(), reset());
    println!("{} {} {}", run.status_glyph(), run.id, run.status);
    println!("  {dim}goal{r}      {}", run.goal);
    println!("  {dim}verifier{r}  {}", run.verifier.describe());
    println!("  {dim}maker{r}     @{}", run.agent);
    if !run.cwd.is_empty() {
        println!("  {dim}folder{r}    {}", run.cwd);
    }
    let budget = run.bounds.budget.map(|b| format!(" \u{b7} {b} tokens")).unwrap_or_default();
    println!(
        "  {dim}bounds{r}    {} iteration(s) \u{b7} {}{budget}",
        run.bounds.max,
        crate::loops::human_age(run.bounds.timeout)
    );
    let p = &run.progress;
    println!(
        "  {dim}spent{r}     {} iteration(s) \u{b7} {} tool call(s) \u{b7} {} in / {} out",
        p.iterations, p.tools, p.input_tokens, p.output_tokens
    );
    // Each line already starts with its iteration number, so it needs no second one.
    for line in &p.tried {
        println!("  {dim}tried{r}     {line}");
    }
    if p.escalated {
        println!("  {dim}escalated{r} a different approach was already asked for");
    }
    if let Some(path) = run.latest_log() {
        println!("  {dim}log{r}       {}", path.display());
    }
    0
}

/// One run's newest iteration, optionally followed while it is still live.
pub(crate) fn loop_log(id: &str, follow: bool) -> i32 {
    let id = match crate::loops::resolve(id) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("aiTerminal: {e}");
            return 2;
        }
    };
    let Some(run) = crate::loops::read(&id) else { return 2 };
    let Some(path) = run.latest_log() else {
        println!("loop {id} has not finished an iteration yet");
        return 0;
    };
    let alive = || matches!(crate::loops::read(&id), Some(r) if r.is_live());
    tail_log(&path, follow, &alive)
}

/// Print a log, then (with `follow`) keep printing what is appended while `live` holds.
pub(crate) fn tail_log(path: &std::path::Path, follow: bool, live: &dyn Fn() -> bool) -> i32 {
    use std::io::{Read, Seek, Write};
    let Ok(mut f) = std::fs::File::open(path) else {
        eprintln!("aiTerminal: can't read {}", path.display());
        return 1;
    };
    let mut text = String::new();
    let _ = f.read_to_string(&mut text);
    print!("{text}");
    let _ = std::io::stdout().flush();
    if !follow {
        return 0;
    }
    let mut at = f.stream_position().unwrap_or(0);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(300));
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.len() > at {
                let _ = f.seek(std::io::SeekFrom::Start(at));
                let mut more = String::new();
                let _ = f.read_to_string(&mut more);
                print!("{more}");
                let _ = std::io::stdout().flush();
                at = meta.len();
            }
        }
        if !live() {
            return 0;
        }
    }
}

/// Clip a single line to `max` display columns, ellipsising the middle-end.
pub(crate) fn clip_tail(s: &str, max: usize) -> String {
    let one_line: String = s.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    if one_line.chars().count() <= max {
        return one_line;
    }
    one_line.chars().take(max.saturating_sub(1)).collect::<String>() + "\u{2026}"
}
