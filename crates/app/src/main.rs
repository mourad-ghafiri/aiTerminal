//! `aiTerminal` — the thin composition root.
//!
//! Everything substantive — the GUI runtime + event loop, the headless renderers,
//! and the CLI subcommands — lives in `framework`. This binary only parses argv
//! and dispatches: subcommands to `framework::cli::*`, headless render modes to
//! `framework::render::*`, and the interactive window to `framework::gui::run`.
//! It names no lower-layer (core/platform) path at all.
//!
//! Everything is a terminal command: the shell integration maps `@ai` /
//! `@<agent>` / `@flow` / `@profile` / `@config` / `@theme` / `@plugin` onto the
//! subcommands below — there is no settings UI.

const DEFAULT_SCRIPT: &str = concat!(
    "printf '\\033[1;38;2;110;155;255maiTerminal\\033[0m  ",
    "\\033[38;2;138;144;160m· an AI-first terminal\\033[0m\\n\\n';",
    "printf '\\033[32m●\\033[0m green  \\033[31m●\\033[0m red  ",
    "\\033[33m●\\033[0m yellow  \\033[36m●\\033[0m cyan  ",
    "\\033[1mbold\\033[0m  \\033[4munderline\\033[0m\\n';",
    "printf 'pipeline: \\033[35mPTY → term → gfx → CoreText\\033[0m\\n';",
    "printf -- '----------------------------------------\\n';",
    "ls -1 / | head -6"
);

struct Args {
    render_ppm: Option<String>,
    render_switcher: bool,
    render_chrome: Option<String>,
    render_icon: Option<String>,
    gen_themes: Option<String>,
    cols: u16,
    rows: u16,
    px: f32,
    cmd: String,
    plugins: Option<String>,
    theme: Option<String>,
}

fn parse_args() -> Args {
    let mut a = Args {
        render_ppm: None,
        render_switcher: false,
        render_chrome: None,
        render_icon: None,
        gen_themes: None,
        cols: 64,
        rows: 14,
        px: 28.0,
        cmd: DEFAULT_SCRIPT.to_string(),
        plugins: None,
        theme: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--render-ppm" => a.render_ppm = it.next(),
            "--render-switcher" => a.render_switcher = true,
            "--render-chrome" => a.render_chrome = it.next(),
            "--render-icon" => a.render_icon = it.next(),
            "--gen-themes" => a.gen_themes = it.next(),
            "--cols" => a.cols = it.next().and_then(|s| s.parse().ok()).unwrap_or(a.cols),
            "--rows" => a.rows = it.next().and_then(|s| s.parse().ok()).unwrap_or(a.rows),
            "--px" => a.px = it.next().and_then(|s| s.parse().ok()).unwrap_or(a.px),
            "--cmd" => {
                if let Some(c) = it.next() {
                    a.cmd = c;
                }
            }
            "--plugins" => a.plugins = it.next(),
            "--theme" => a.theme = it.next(),
            _ => {}
        }
    }
    a
}

fn main() {
    // Log every panic (thread + payload + location) to stderr + ~/.aiTerminal/crash.log,
    // so a panic caught by the event-loop resilience boundary (a dropped frame, not an abort)
    // is still diagnosable.
    framework::gui::install_panic_hook();

    // Subcommands (`aiTerminal <cmd> …`) — manage declarative plugins/config/
    // themes/profiles, or run the offline-capable AI CLI. Each returns an exit code.
    let raw: Vec<String> = std::env::args().collect();
    match raw.get(1).map(String::as_str) {
        Some("plugin") => std::process::exit(framework::cli::plugin(&raw[2..])),
        Some("config") => std::process::exit(framework::cli::config(&raw[2..])),
        Some("theme") => std::process::exit(framework::cli::theme(&raw[2..])),
        Some("profile") => std::process::exit(framework::cli::profile(&raw[2..])),
        Some("ai") => std::process::exit(framework::cli::ai(&raw[2..])),
        _ => {}
    }

    let args = parse_args();

    // `--render-chrome <pos> [--render-ppm <out>] [--theme <name>]` renders the window
    // chrome (status + tab bar) in a tab-bar orientation: top | bottom | left | right.
    if let Some(pos) = args.render_chrome.clone() {
        let out = args.render_ppm.clone().unwrap_or_else(|| format!("/tmp/chrome-{pos}.ppm"));
        guard(framework::render::render_chrome(&pos, args.theme.as_deref(), &out), "chrome render failed");
        return;
    }

    // `--render-switcher [--render-ppm <out>]` renders the tab quick-switcher overlay.
    if args.render_switcher {
        let out = args.render_ppm.clone().unwrap_or_else(|| "/tmp/switcher.ppm".into());
        guard(framework::gui::render_switcher_proof(&out), "switcher render failed");
        return;
    }

    // `--gen-themes <dir>` writes the built-in theme collection as TOML (dev tooling).
    if let Some(dir) = args.gen_themes.clone() {
        guard(framework::theme::write_collection(std::path::Path::new(&dir)), "theme generation failed");
        println!("wrote the theme collection to {dir}");
        return;
    }

    // `--render-icon <out.png>` draws the app icon (used by the bundle script).
    if let Some(path) = args.render_icon.clone() {
        guard(framework::render::render_icon(&path), "icon render failed");
        return;
    }

    match &args.render_ppm {
        Some(path) => {
            let r = framework::render::TerminalRender {
                cols: args.cols,
                rows: args.rows,
                px: args.px,
                cmd: args.cmd.clone(),
                plugins: args.plugins.clone(),
                theme: args.theme.clone(),
            };
            guard(framework::render::headless_render(&r, path), "render failed");
        }
        None => framework::gui::run(framework::config::Config::load()),
    }
}

/// Print the renderer's error to stderr and exit non-zero, or fall through on Ok.
fn guard(result: std::io::Result<()>, what: &str) {
    if let Err(e) = result {
        eprintln!("aiTerminal: {what}: {e}");
        std::process::exit(1);
    }
}
