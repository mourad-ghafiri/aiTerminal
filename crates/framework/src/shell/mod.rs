//! Shell integration — the GENERIC engine that assembles what the terminal injects
//! into the shell it spawns. It is deliberately dumb about *features*: every behaviour
//! (completion, autosuggestions, history, the prompt, alias hints, …) is a **builtin
//! plugin** that ships a `shell.zsh` / `shell.bash` snippet; this engine only:
//!   - renders the plugins' declarative **aliases** + **abbreviations**,
//!   - exports the **context** snippets need (theme colors as `TT_*` shell vars, the
//!     alias index `TT_ALIAS_BY_HEAD`, and theme-derived `LS_COLORS`),
//!   - **sources each enabled, trusted plugin's snippet**,
//!   - and wires it in non-destructively (ZDOTDIR for zsh / `--rcfile` for bash) so your
//!     real shell config always loads first.
//!
//! So features are added/removed by enabling/disabling **plugins** — nothing here is
//! feature-specific. Snippets are shell CODE, so [`PluginRegistry::shell_snippets`]
//! returns them only for TRUSTED plugins (like the `exec` primitive). Per-spawn
//! regeneration means a new tab reflects the current theme + plugin set. Opt out
//! entirely with `[shell] integration = false`. Generation is pure (unit-tested); only
//! [`prepare`] touches the disk.

use std::path::Path;

use corelib::theme::Theme;
use corelib::types::Rgba8;

use crate::config::Config;
use crate::plugin::{CompletionSpec, PluginRegistry};

/// How to spawn the shell: env overrides + extra argv + whether to launch as a login
/// shell. `bare()` is "no integration".
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ShellSpawn {
    pub env: Vec<(String, String)>,
    pub args: Vec<String>,
    pub login: bool,
}

impl ShellSpawn {
    fn bare() -> Self {
        ShellSpawn { env: Vec::new(), args: Vec::new(), login: true }
    }
}

/// The recognised shells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShellKind {
    Zsh,
    Bash,
    Other,
}

impl ShellKind {
    fn detect(shell: &str) -> ShellKind {
        let path = if shell.trim().is_empty() { std::env::var("SHELL").unwrap_or_default() } else { shell.to_string() };
        match path.rsplit('/').next().unwrap_or("") {
            "zsh" => ShellKind::Zsh,
            "bash" => ShellKind::Bash,
            _ => ShellKind::Other,
        }
    }
}

/// What a dialect assembles: the plugins' alias / abbr / completion data and the
/// trusted plugins' shell snippets (`(plugin name, code)`). Theme colors are NOT
/// inlined — they live in the shared, live-refreshed colors file.
pub(crate) struct Integration {
    pub(crate) aliases: Vec<(String, String)>,
    pub(crate) abbrs: Vec<(String, String)>,
    pub(crate) completions: Vec<CompletionSpec>,
    pub(crate) snippets: Vec<(String, String)>,
}

/// Per-shell init strategy.
trait Dialect {
    /// Write this shell's init files under `dir` and return how to spawn it.
    fn prepare(&self, dir: &Path, ctx: &Integration) -> ShellSpawn;
}

/// Prepare the shell to spawn: (re)write the integration files and return the
/// env/args/login. [`ShellSpawn::bare`] when integration is off; an unsupported shell
/// still gets the themed `ls` colors via env.
pub fn prepare(config: &Config, registry: &PluginRegistry, theme: &Theme, shell: &str) -> ShellSpawn {
    if !config.shell_integration {
        return ShellSpawn::bare();
    }
    let kind = ShellKind::detect(shell);
    if kind == ShellKind::Other {
        return ShellSpawn { env: color_env(theme), args: Vec::new(), login: true };
    }
    let ctx = Integration {
        aliases: registry.aliases(),
        abbrs: registry.abbreviations(),
        completions: registry.completions(),
        snippets: registry.shell_snippets(kind == ShellKind::Bash),
    };
    let dir = Config::dir().join("shell");
    if let Err(e) = write_colors_file(theme) {
        platform::warn!("failed to write shell colors: {e}");
    }
    let mut spawn = match kind {
        ShellKind::Zsh => Zsh.prepare(&dir, &ctx),
        ShellKind::Bash => Bash.prepare(&dir, &ctx),
        ShellKind::Other => unreachable!(),
    };
    // File-type `ls` colors travel as env, so every shell + child process gets them.
    let mut env = color_env(theme);
    // Expose the absolute path to THIS binary so the shell integration (the `@ai` plugin's
    // command_not_found handler + the `aiTerminal` completions) can invoke the CLI even
    // when launched from the macOS .app bundle, where `<bundle>/Contents/MacOS/` is NOT on
    // PATH (the reported `@ai: command not found: aiTerminal`). Snippets use
    // `"${TT_BIN:-aiTerminal}"` so a PATH-install / dev run / hand-sourced snippet still
    // works. `current_exe()` resolves to the bundle binary, the same anchor `Config` uses
    // for builtin-registry resolution.
    if let Ok(exe) = std::env::current_exe() {
        env.push(("TT_BIN".to_string(), exe.to_string_lossy().into_owned()));
    }
    // Point the `@ai`/agent CLI at the host's redacted terminal-session file, so it can
    // ground on the recent session ("go into it"). Only injected when sharing is on, so
    // the CLI can't read it when `[ai] share_terminal_context = false`.
    if config.ai_share_terminal_context {
        env.push(("TT_SESSION_LOG".to_string(), Config::session_context_path().to_string_lossy().into_owned()));
    }
    env.append(&mut spawn.env);
    spawn.env = env;
    spawn
}

// ===== the live colors file ================================================

/// The path of the shared colors file (`~/.aiTerminal/shell/colors.sh`).
pub fn colors_path() -> std::path::PathBuf {
    Config::dir().join("shell").join("colors.sh")
}

/// The SOLID selection-band color for `theme`: a neutral gray — the terminal
/// foreground blended ~30% over the background — clearly visible on any theme
/// without tinting or hiding the selected text (an accent-tinted band made
/// colored commands hard to read). Shared by the shell export (`TT_SEL_BG`,
/// zsh's region highlight), the mouse-selection overlay, and the workspace
/// snapshot scrub that keeps the band out of saved terminal content.
pub(crate) fn selection_band(theme: &Theme) -> corelib::types::Rgba8 {
    let (fg, bg) = (theme.term_fg, theme.term_bg);
    // 50% fg over bg: a LIGHT, unmistakable gray on dark themes (and a clear
    // dark gray on light ones) — 30% proved too subtle to notice.
    let mix = |f: u8, b: u8| ((f as u32 + b as u32) / 2) as u8;
    corelib::types::Rgba8::rgb(mix(fg.r, bg.r), mix(fg.g, bg.g), mix(fg.b, bg.b))
}

/// Write the theme's shell colors — `TT_*` prompt/hint vars + themed `LS_COLORS`/
/// `LSCOLORS` — as a POSIX `export` file BOTH dialects source. It is re-sourced by
/// every running shell at its next prompt (a cheap `-nt` mtime check), so a live
/// `@theme` switch recolors the prompt, syntax highlighting, and subsequent `ls`
/// output without respawning anything.
pub fn write_colors_file(theme: &Theme) -> std::io::Result<()> {
    let dir = Config::dir().join("shell");
    std::fs::create_dir_all(&dir)?;
    let mut s = header();
    s.push_str("# The active theme's shell colors — rewritten on every theme change.\n");
    for (name, c) in [
        ("FG", theme.fg),
        ("MUTED", theme.muted),
        ("ACCENT", theme.accent),
        ("ACCENT2", theme.accent2()),
        ("SUCCESS", theme.success),
        ("WARN", theme.warn),
        ("ERROR", theme.error),
    ] {
        s.push_str(&format!("export TT_{name}='{}'\nexport TT_{name}_RGB='{}'\n", hex(c), rgb(c)));
    }
    let sel = selection_band(theme);
    s.push_str(&format!("export TT_SEL_BG='{}'\nexport TT_SEL_BG_RGB='{}'\n", hex(sel), rgb(sel)));
    for (k, v) in color_env(theme) {
        s.push_str(&format!("export {k}={}\n", sh_squote(&v)));
    }
    std::fs::write(dir.join("colors.sh"), s)
}

/// The integration block that sources [`colors_path`] now and re-sources it at
/// every prompt when its mtime moved (`-nt` against a per-session marker file —
/// no `stat` portability games). Works verbatim in zsh and bash.
fn colors_source_block() -> String {
    let path = sh_squote(&colors_path().to_string_lossy());
    format!(
        "\n# theme colors — sourced from a FILE so a live `@theme` switch recolors this\n\
         # running shell at its next prompt (prompt, hints, ls colors).\n\
         _tt_colors={path}\n\
         _tt_colors_seen=\"${{TMPDIR:-/tmp}}/tt-colors-seen.$$\"\n\
         [ -r \"$_tt_colors\" ] && . \"$_tt_colors\"\n\
         : > \"$_tt_colors_seen\"\n\
         _tt_refresh_colors() {{\n\
           [ \"$_tt_colors\" -nt \"$_tt_colors_seen\" ] || return 0\n\
           . \"$_tt_colors\" 2>/dev/null\n\
           : > \"$_tt_colors_seen\"\n\
         }}\n"
    )
}

// ===== assembly (dialect-agnostic) =========================================

/// The banner written atop every generated shell-integration file. Derived from the
/// one brand constant so a rename flows through (the name + the `~/.<brand>/config.toml`
/// hint both follow `corelib::brand::NAME`).
fn header() -> String {
    let name = corelib::brand::NAME;
    format!("# {name} shell integration — generated; do not edit.\n# Disable with `[shell] integration = false` in ~/.{name}/config.toml.\n")
}

/// The integration body sourced from our rc: theme-color vars + the alias reverse-map
/// (context for plugin snippets), the declarative aliases + abbreviations, then each
/// enabled plugin's snippet (the features).
pub(crate) fn zsh_integration(ctx: &Integration) -> String {
    let mut s = header();
    s.push_str(&colors_source_block());
    s.push_str("typeset -ga precmd_functions\n(( ${precmd_functions[(I)_tt_refresh_colors]} )) || precmd_functions+=(_tt_refresh_colors)\n");
    s.push_str(&tt_alias_index(&ctx.aliases));
    s.push_str(&tt_completion_map(&ctx.completions));
    s.push_str("\n# --- aliases (from your enabled plugins) ---\n");
    s.push_str(&alias_lines(&ctx.aliases));
    // Zsh GLOBAL aliases (common's `H` = `| head`, …) expand ANYWHERE during
    // parse — including inside a later snippet's source, where a bare `H`
    // becomes a pipe and aborts the whole file with a parse error. Snippets
    // are therefore parsed with alias expansion off (defining aliases still
    // works; interactive use at the prompt is untouched — expansion is
    // restored right after).
    s.push_str("\n[[ -o aliases ]] && __tt_aliases_on=1 || __tt_aliases_on=0\nbuiltin setopt no_aliases\n");
    s.push_str(&zsh_abbr_block(&ctx.abbrs));
    s.push_str(&plugin_snippets(&ctx.snippets));
    s.push_str("\n(( __tt_aliases_on )) && builtin setopt aliases\nbuiltin unset __tt_aliases_on\n");
    s
}

pub(crate) fn bash_integration(ctx: &Integration) -> String {
    let mut s = header();
    s.push_str(&colors_source_block());
    s.push_str("case \";${PROMPT_COMMAND};\" in *\";_tt_refresh_colors;\"*) ;; *) PROMPT_COMMAND=\"_tt_refresh_colors${PROMPT_COMMAND:+;$PROMPT_COMMAND}\" ;; esac\n");
    s.push_str(&tt_alias_index(&ctx.aliases));
    s.push_str("\n# --- aliases (from your enabled plugins) ---\n");
    s.push_str(&alias_lines(&ctx.aliases));
    s.push_str(&plugin_snippets(&ctx.snippets));
    s
}

/// The alias **forward index** for the alias-hints plugin: every alias bucketed by the
/// **head token** of its expansion, each bucket sorted **longest-expansion-first** (then
/// shorter alias name). The shell hint walks a bucket and suggests the LONGEST alias whose
/// expansion is a token-prefix of what you typed — so `git commit -m "msg"` → `gcm`,
/// regardless of args/quoting. Emitted as one assoc-array subscript assignment per head,
/// the value a `\n`-joined list of `name\texpansion` rows, ANSI-C quoted so the form is
/// identical in zsh and bash (both expand `$'…\t…\n…'`).
fn tt_alias_index(aliases: &[(String, String)]) -> String {
    use std::collections::BTreeMap;
    // head token → [(alias name, full expansion, expansion token count)]
    let mut buckets: BTreeMap<&str, Vec<(&str, &str, usize)>> = BTreeMap::new();
    for (name, value) in aliases {
        if !ident(name) {
            continue;
        }
        let v = value.trim();
        // Skip empties, self-aliases, and aliases that don't actually shorten typing.
        if v.is_empty() || v == name || name.len() >= v.len() {
            continue;
        }
        let toks: Vec<&str> = v.split_whitespace().collect();
        let head = match toks.first() {
            // Only index heads that are safe as an assoc-array subscript / command word.
            Some(h) if ident_cmd(h) => *h,
            _ => continue,
        };
        buckets.entry(head).or_default().push((name, v, toks.len()));
    }
    if buckets.is_empty() {
        return String::new();
    }
    // `2>/dev/null` keeps ancient bash (3.2, no associative arrays — the macOS default)
    // silent; its alias-hints snippet is gated on bash ≥ 4, so it simply never reads this.
    let mut s = String::from(
        "\n# alias index (expansion head → longest-first 'name\\texpansion' rows, for alias-hints)\ntypeset -gA TT_ALIAS_BY_HEAD 2>/dev/null\n",
    );
    for (head, mut entries) in buckets {
        // Longest expansion first so the shell's first prefix match is the longest;
        // tie-break on the shorter alias name (the bigger keystroke saving).
        entries.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.len().cmp(&b.0.len())).then_with(|| a.0.cmp(b.0)));
        let mut rows = String::new();
        for (name, expn, _) in entries {
            if !rows.is_empty() {
                rows.push('\n');
            }
            rows.push_str(name);
            rows.push('\t');
            rows.push_str(expn);
        }
        // The key is emitted UNQUOTED: zsh stores `a['git']=…` under the literal key
        // `'git'` (quotes included), so a later `${a[git]}` lookup would miss. `head` is
        // `ident_cmd`-safe (alnum/`_`/`-`/`.`), so it needs no quoting in a subscript, and
        // the unquoted form is the one bash + zsh agree on.
        s.push_str(&format!("TT_ALIAS_BY_HEAD[{head}]={}\n", ansi_c_quote(&rows)));
    }
    s
}

/// The plugins' declarative `[[completion]]` specs as two zsh assoc arrays
/// (`command → "sub1 sub2 …"` / `command → "--flag1 --flag2 …"`), so the completion
/// plugin can register a `compdef` for custom commands zsh doesn't already know — add
/// tab-completion for any tool with pure data, no zsh authoring.
fn tt_completion_map(completions: &[CompletionSpec]) -> String {
    // Keep only commands that contribute at least one candidate.
    let usable: Vec<&CompletionSpec> = completions
        .iter()
        .filter(|c| ident_cmd(&c.command) && (!c.subcommands.is_empty() || !c.flags.is_empty()))
        .collect();
    if usable.is_empty() {
        return String::new();
    }
    let words = |xs: &[String]| xs.iter().map(|x| x.trim()).filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" ");
    let mut sub = String::new();
    let mut flags = String::new();
    for c in &usable {
        sub.push_str(&format!(" {} {}", sh_squote(&c.command), sh_squote(&words(&c.subcommands))));
        flags.push_str(&format!(" {} {}", sh_squote(&c.command), sh_squote(&words(&c.flags))));
    }
    format!(
        "\n# declarative completion specs (consumed by the completion plugin)\n\
         typeset -gA TT_COMPL_SUB TT_COMPL_FLAGS\nTT_COMPL_SUB=({sub} )\nTT_COMPL_FLAGS=({flags} )\n"
    )
}

/// A shell command name safe to `compdef` (letters/digits/`_`/`-`/`.`, not empty).
fn ident_cmd(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// Concatenate the trusted plugins' shell snippets, each under a `# --- plugin: <name>`
/// header. The engine knows nothing about what they do.
fn plugin_snippets(snippets: &[(String, String)]) -> String {
    let mut s = String::new();
    for (name, code) in snippets {
        s.push_str(&format!("\n# --- plugin: {name} ---\n"));
        s.push_str(code);
        if !code.ends_with('\n') {
            s.push('\n');
        }
    }
    s
}

/// The expand-on-space widget for the plugins' declarative `[[abbr]]` data.
fn zsh_abbr_block(abbrs: &[(String, String)]) -> String {
    let valid: Vec<&(String, String)> = abbrs.iter().filter(|(k, _)| ident(k)).collect();
    if valid.is_empty() {
        return String::new();
    }
    let mut s = String::from("\n# --- abbreviations (expand on space) ---\ntypeset -gA __tt_abbr\n");
    for (k, v) in valid {
        s.push_str(&format!("__tt_abbr[{k}]={}\n", sh_squote(v)));
    }
    s.push_str(
        "__tt_abbr_expand() {\n  emulate -L zsh\n  local word=${LBUFFER##*[[:space:]]}\n  local exp=${__tt_abbr[$word]}\n  [[ -n $exp ]] && LBUFFER=${LBUFFER%$word}$exp\n  zle self-insert\n}\nzle -N __tt_abbr_expand\nbindkey ' ' __tt_abbr_expand\n",
    );
    s
}

/// Where the user's *own* zsh config lives — never our generated directory.
///
/// This is the whole bug, and it only bites when the app is launched from inside one of
/// its own shells, which is exactly what happens all day while developing it. We set
/// `ZDOTDIR` to our directory for every shell we spawn; reading `ZDOTDIR` back as "the
/// user's real one" therefore yields *ours*, and the generated files go on to source
/// themselves without end.
///
/// So: trust an inherited `TT_REAL_ZDOTDIR` first (it is what a parent already worked
/// out), fall back to `ZDOTDIR`, and reject either the moment it names our own tree.
/// Idempotent at any nesting depth, because the answer can only ever be a directory that
/// is not `zdir`.
fn real_zdotdir(zdir: &Path) -> String {
    let ours = |p: &str| {
        let p = Path::new(p);
        p == zdir || p.starts_with(zdir)
    };
    ["TT_REAL_ZDOTDIR", "ZDOTDIR"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.trim().is_empty() && !ours(v))
        .unwrap_or_else(home)
}

// ===== zsh =================================================================

struct Zsh;

impl Dialect for Zsh {
    fn prepare(&self, dir: &Path, ctx: &Integration) -> ShellSpawn {
        let zdir = dir.join("zsh");
        let _ = std::fs::create_dir_all(&zdir);
        let integ = zdir.join("integration.zsh");
        // Chain the user's real config (sourced from $TT_REAL_ZDOTDIR), then ours.
        //
        // The `!=` is not paranoia. If `TT_REAL_ZDOTDIR` ever names THIS directory, each
        // of these files sources itself, forever — zsh reports it as "job table full or
        // recursion limit exceeded", in every pane, and the shell never starts. The Rust
        // side below makes that unreachable; this guard makes a file already written with
        // the old value stop recursing the moment it is read, without waiting to be
        // regenerated.
        let header = header();
        let own = sh_squote(&zdir.to_string_lossy());
        let guard = format!("TT_OWN_ZDOTDIR={own}\n: \"${{TT_REAL_ZDOTDIR:=$HOME}}\"\n");
        // An `if` rather than `[ … ] && source`, so the file's exit status is 0 when there
        // is nothing to chain. The old form returned 1 whenever the user had no such file,
        // which makes `source .zshenv && …` fail for a reason that is not a failure.
        let chain = |hook: &str| {
            format!(
                "if [ \"$TT_REAL_ZDOTDIR\" != \"$TT_OWN_ZDOTDIR\" ] && [ -r \"$TT_REAL_ZDOTDIR/{hook}\" ]; then\n  source \"$TT_REAL_ZDOTDIR/{hook}\"\nfi\n"
            )
        };
        let _ = std::fs::write(zdir.join(".zshenv"), format!("{header}{guard}{}", chain(".zshenv")));
        let _ = std::fs::write(zdir.join(".zprofile"), format!("{header}{guard}{}", chain(".zprofile")));
        let _ = std::fs::write(zdir.join(".zlogin"), format!("{header}{guard}{}", chain(".zlogin")));
        // .zshrc: snapshot the default PROMPT (so the prompt plugin can avoid clobbering
        // a custom one), source the user's, then our integration.
        let zshrc = format!(
            "{header}{guard}__tt_prompt_default=\"$PROMPT\"\n{}source {}\n",
            chain(".zshrc"),
            sh_squote(&integ.to_string_lossy()),
        );
        let _ = std::fs::write(zdir.join(".zshrc"), zshrc);
        if let Err(e) = std::fs::write(&integ, zsh_integration(ctx)) {
            platform::warn!("failed to write zsh integration {}: {e}", integ.display());
        }

        ShellSpawn {
            env: vec![
                ("ZDOTDIR".into(), zdir.to_string_lossy().into_owned()),
                ("TT_REAL_ZDOTDIR".into(), real_zdotdir(&zdir)),
            ],
            args: Vec::new(),
            login: true,
        }
    }
}

// ===== bash ================================================================

struct Bash;

impl Dialect for Bash {
    fn prepare(&self, dir: &Path, ctx: &Integration) -> ShellSpawn {
        let bdir = dir.join("bash");
        let _ = std::fs::create_dir_all(&bdir);
        let init = bdir.join("init.bash");
        if let Err(e) = std::fs::write(&init, bash_integration(ctx)) {
            platform::warn!("failed to write bash integration {}: {e}", init.display());
        }
        // An interactive (non-login) bash via --rcfile: replicate login sourcing
        // ourselves (profile → bashrc), snapshot the default PS1, then layer ours.
        let rc = format!(
            "{}__tt_ps1_default=\"$PS1\"\n[ -r /etc/profile ] && source /etc/profile\n\
             if [ -r \"$HOME/.bash_profile\" ]; then source \"$HOME/.bash_profile\";\n\
             elif [ -r \"$HOME/.bashrc\" ]; then source \"$HOME/.bashrc\"; fi\nsource {}\n",
            header(),
            sh_squote(&init.to_string_lossy()),
        );
        let rcfile = bdir.join("bashrc");
        let _ = std::fs::write(&rcfile, rc);
        ShellSpawn {
            env: Vec::new(),
            args: vec!["--rcfile".into(), rcfile.to_string_lossy().into_owned()],
            login: false,
        }
    }
}

// ===== file-type `ls` colors (env, dialect-agnostic) =======================

/// Theme-derived file-type `ls` colors as env, so any shell + child process colorizes
/// paths to match the theme: `LS_COLORS` (GNU `gls`, truecolor + per-extension),
/// `LSCOLORS` (BSD — the macOS default `ls`), and `CLICOLOR=1`.
fn color_env(theme: &Theme) -> Vec<(String, String)> {
    let f = theme.files();
    let tc = |c: Rgba8| format!("38;2;{};{};{}", c.r, c.g, c.b);

    // GNU LS_COLORS — truecolor per type, then per-extension groups.
    let mut gnu = format!(
        "di={di}:ln={ln}:ex={ex}:or={or}:mi={or}:so={me}:pi={cf}:bd={ar}:cd={ar}",
        di = tc(f.directory),
        ln = tc(f.symlink),
        ex = tc(f.executable),
        or = tc(f.broken),
        me = tc(f.media),
        cf = tc(f.config),
        ar = tc(f.archive),
    );
    let group = |exts: &[&str], c: Rgba8| {
        let col = tc(c);
        exts.iter().map(|e| format!(":*.{e}={col}")).collect::<String>()
    };
    gnu.push_str(&group(&["zip", "tar", "tgz", "gz", "bz2", "xz", "zst", "7z", "rar", "jar", "deb", "rpm", "dmg"], f.archive));
    gnu.push_str(&group(&["png", "jpg", "jpeg", "gif", "bmp", "svg", "webp", "ico", "tiff", "heic"], f.image));
    gnu.push_str(&group(&["mp3", "wav", "flac", "aac", "ogg", "m4a", "aiff", "mp4", "mkv", "mov", "webm", "avi", "m4v"], f.media));
    gnu.push_str(&group(&["rs", "py", "js", "ts", "tsx", "jsx", "c", "h", "cpp", "hpp", "cc", "go", "java", "rb", "php", "swift", "kt", "lua", "sql"], f.code));
    gnu.push_str(&group(&["sh", "bash", "zsh", "fish"], f.executable));
    gnu.push_str(&group(&["toml", "yaml", "yml", "json", "ini", "conf", "cfg", "lock", "env"], f.config));
    gnu.push_str(&group(&["md", "markdown", "txt", "rst", "pdf", "doc", "docx", "org", "tex"], f.document));

    // BSD LSCOLORS — 11 fg/bg pairs (bg = `x` default), each fg an ANSI letter (a..h)
    // nearest the themed color (BSD `ls` then renders it via the theme's ANSI palette).
    let letter = |c: Rgba8| (b'a' + nearest_ansi8(theme, c)) as char;
    let slots = [
        f.directory, f.symlink, f.media, f.config, f.executable, // dir, link, socket, pipe, exec
        f.archive, f.archive, f.executable, f.executable, f.directory, f.directory, // block, char, setuid, setgid, sticky-ow, ow
    ];
    let bsd: String = slots.iter().flat_map(|c| [letter(*c), 'x']).collect();

    vec![("CLICOLOR".into(), "1".into()), ("LSCOLORS".into(), bsd), ("LS_COLORS".into(), gnu)]
}

/// Index (0..8) of the base ANSI color nearest `c` in the theme's palette.
fn nearest_ansi8(theme: &Theme, c: Rgba8) -> u8 {
    let mut best = 0u8;
    let mut bd = i64::MAX;
    for i in 0..8u8 {
        let a = theme.ansi(i);
        let d = (a.r as i64 - c.r as i64).pow(2) + (a.g as i64 - c.g as i64).pow(2) + (a.b as i64 - c.b as i64).pow(2);
        if d < bd {
            bd = d;
            best = i;
        }
    }
    best
}

// ===== small helpers =======================================================

fn home() -> String {
    platform::os::home_dir().map(|p| p.display().to_string()).unwrap_or_else(|| ".".into())
}

/// A safe shell identifier for an alias/abbr name (skip anything exotic).
fn ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Single-quote a value for POSIX shells (`'` → `'\''`).
fn sh_squote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// ANSI-C quote a value (`$'…'`) — the one quoting form bash and zsh share that encodes
/// embedded tabs/newlines, so a single emitted index parses identically in both shells.
fn ansi_c_quote(s: &str) -> String {
    let mut out = String::from("$'");
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// `#RRGGBB` (for zsh `%F{#…}`).
fn hex(c: Rgba8) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
}
/// `r;g;b` (for bash `\e[38;2;r;g;bm`).
fn rgb(c: Rgba8) -> String {
    format!("{};{};{}", c.r, c.g, c.b)
}

fn alias_lines(aliases: &[(String, String)]) -> String {
    let mut s = String::new();
    for (k, v) in aliases {
        if ident(k) {
            s.push_str(&format!("alias {k}={}\n", sh_squote(v)));
        }
    }
    s
}

#[cfg(test)]
mod tests;
