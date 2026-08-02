use super::*;

/// Every `[section]` a config document can carry, and the function that applies it.
/// Adding one is a line here and a function below — never an edit to a single long
/// walk over the whole document.
type Section = fn(&mut Config, &Toml);
const SECTIONS: [(&str, Section); 16] = [
    ("appearance", apply_appearance),
    ("behavior", apply_behavior),
    ("md", apply_md),
    ("jobs", apply_jobs),
    ("loop", apply_loop),
    ("flow", apply_flow),
    ("motivation", apply_motivation),
    ("ai", apply_ai),
    ("gates", apply_gates),
    ("plugins", apply_plugins),
    ("shell", apply_shell),
    ("registry", apply_registry),
    ("logging", apply_logging),
    ("security", apply_security),
    ("redact", apply_redact),
    ("keybinding", apply_keybinding),
];

impl Config {
    /// Apply a config document's *present* keys onto `self` (absent keys keep their current
    /// value). This is the overlay primitive behind profiles: parse the global `config.toml`
    /// into a [`Config`], then `apply_toml` a profile's `config.toml` on top, so everything the
    /// profile declares overrides the global. A profile that declares any `[[ai.model]]`
    /// REPLACES the inherited pool (not merged); scalars/maps override in place; the
    /// `keybinding`/`redact` lists append (the keymap is "later wins", redaction is additive).
    pub(crate) fn apply_toml(&mut self, text: &str) {
        // A syntax error collapses the WHOLE document to empty, silently reverting every
        // setting to its default — warn so the user learns their config wasn't applied
        // (rather than mysteriously losing all customization to one stray bracket).
        let doc = match Toml::parse(text) {
            Ok(d) => d,
            Err(e) => {
                platform::warn!("config.toml parse error — using defaults for this document: {e}");
                Toml::Table(Vec::new())
            }
        };

        for (key, apply) in SECTIONS {
            if let Some(section) = doc.get(key) {
                apply(self, section);
            }
        }
    }
}

fn chat_id(v: &Toml) -> Option<String> {
    v.as_int()
        .map(|n| n.to_string())
        .or_else(|| v.as_str().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
}

fn apply_appearance(c: &mut Config, a: &Toml) {
    if let Some(v) = a.get("theme").and_then(|v| v.as_str()) {
        c.theme = v.to_string();
    }
    if let Some(v) = a.get("locale").and_then(|v| v.as_str()) {
        if !v.trim().is_empty() {
            c.locale = v.to_string();
        }
    }
    if let Some(v) = a.get("font_family").and_then(|v| v.as_str()) {
        if !v.trim().is_empty() {
            c.font_family = v.to_string();
        }
    }
    if let Some(v) = a.get("font_size").and_then(|v| v.as_num()).filter(|v| v.is_finite()) {
        c.font_size = (v as f32).clamp(6.0, 96.0);
    }
    if let Some(v) = a.get("cursor_style").and_then(|v| v.as_str()) {
        if !v.trim().is_empty() {
            c.cursor_style = v.to_string();
        }
    }
}

fn apply_behavior(c: &mut Config, b: &Toml) {
    if let Some(v) = b.get("zoom").and_then(|v| v.as_num()).filter(|v| v.is_finite()) {
        c.zoom = (v as f32).clamp(0.4, 3.0);
    }
    if let Some(v) = b.get("tab_bar").and_then(|v| v.as_str()) {
        c.tab_bar = v.to_string();
    }
    if let Some(v) = b.get("shell").and_then(|v| v.as_str()) {
        c.shell = v.to_string();
    }
    if let Some(v) = b.get("scrollback").and_then(|v| v.as_int()) {
        // Clamp BOTH ends: a config typo (`scrollback = 999999999999`) must not drive a
        // multi-gigabyte buffer allocation. 1M lines is already far beyond any real use.
        c.scrollback = v.clamp(0, 1_000_000) as usize;
    }
    if let Some(v) = b.get("confirm_close_pane").and_then(|v| v.as_bool()) {
        c.confirm_close_pane = v;
    }
    if let Some(v) = b.get("confirm_close_tab").and_then(|v| v.as_bool()) {
        c.confirm_close_tab = v;
    }
    if let Some(v) = b.get("confirm_quit").and_then(|v| v.as_bool()) {
        c.confirm_quit = v;
    }
}

fn apply_md(c: &mut Config, md: &Toml) {
    if let Some(v) = md.get("remote_images").and_then(|v| v.as_bool()) {
        c.md_remote_images = v;
    }
    if let Some(v) = md.get("image_max_rows").and_then(|v| v.as_int()) {
        c.md_image_max_rows = v.clamp(1, 200) as usize;
    }
    if let Some(v) = md.get("syntax").and_then(|v| v.as_bool()) {
        c.md_syntax = v;
    }
}

fn apply_jobs(c: &mut Config, j: &Toml) {
    if let Some(v) = j.get("max_concurrent").and_then(|v| v.as_int()) {
        c.jobs_max_concurrent = v.clamp(1, 64) as usize;
    }
    if let Some(v) = j.get("keep_runs").and_then(|v| v.as_int()) {
        c.jobs_keep_runs = v.clamp(1, 500) as usize;
    }
    if let Some(v) = j.get("max_log_bytes").and_then(|v| v.as_int()) {
        c.jobs_max_log_bytes = v.clamp(4096, 1 << 30) as u64;
    }
}

fn apply_loop(c: &mut Config, l: &Toml) {
    if let Some(v) = l.get("max").and_then(|v| v.as_int()) {
        c.loop_max = v.clamp(1, 25) as u32;
    }
    // Durations are written the way people write them (`30m`); an unreadable one
    // keeps the default rather than silently becoming zero (= no bound at all).
    if let Some(v) = l.get("timeout").and_then(|v| v.as_str()).and_then(corelib::datetime::duration) {
        c.loop_timeout = v.clamp(30, 24 * 3600);
    }
    if let Some(v) = l.get("check_timeout").and_then(|v| v.as_str()).and_then(corelib::datetime::duration) {
        c.loop_check_timeout = v.clamp(5, 3600);
    }
    if let Some(v) = l.get("keep_runs").and_then(|v| v.as_int()) {
        c.loop_keep_runs = v.clamp(1, 500) as usize;
    }
    if let Some(v) = l.get("propose_check").and_then(|v| v.as_bool()) {
        c.loop_propose_check = v;
    }
}

fn apply_flow(c: &mut Config, f: &Toml) {
    if let Some(v) = f.get("concurrency").and_then(|v| v.as_int()) {
        c.flow_concurrency = v.clamp(1, 16) as usize;
    }
    if let Some(v) = f.get("timeout").and_then(|v| v.as_str()).and_then(corelib::datetime::duration) {
        c.flow_timeout = v.clamp(30, 24 * 3600);
    }
    if let Some(v) = f.get("node_timeout").and_then(|v| v.as_str()).and_then(corelib::datetime::duration) {
        c.flow_node_timeout = v.clamp(5, 24 * 3600);
    }
    if let Some(v) = f.get("keep_runs").and_then(|v| v.as_int()) {
        c.flow_keep_runs = v.clamp(1, 500) as usize;
    }
    if let Some(v) = f.get("max_map").and_then(|v| v.as_int()) {
        c.flow_max_map = v.clamp(1, 256) as usize;
    }
    // `[flow] view = "graph"|"list"`. Only the two words this understands are
    // taken; anything else leaves the default standing rather than turning a
    // typo into a board nobody asked for.
    if let Some(v) = f.get("view").and_then(|v| v.as_str()) {
        if matches!(v.trim().to_ascii_lowercase().as_str(), "graph" | "list") {
            c.flow_view = v.trim().to_ascii_lowercase();
        }
    }
}

/// `[motivation]` — the line shown beside a spinner while you wait.
///
/// Every key is clamped rather than trusted: `after = "0s"` would put a line up before
/// the run has drawn breath, and `every = "1s"` would flicker one row of the terminal at
/// reading speed. Both are the difference between a feature and an irritation.
fn apply_motivation(c: &mut Config, m: &Toml) {
    if let Some(v) = m.get("enabled").and_then(|v| v.as_bool()) {
        c.motivation_enabled = v;
    }
    if let Some(v) = m.get("after").and_then(|v| v.as_str()).and_then(corelib::datetime::duration) {
        c.motivation_after = v.clamp(2, 120);
    }
    if let Some(v) = m.get("every").and_then(|v| v.as_str()).and_then(corelib::datetime::duration) {
        c.motivation_every = v.clamp(5, 600);
    }
    // An empty list is a real answer — "none of them" — and is how somebody turns the
    // whole thing off without arguing with `enabled`. A word nobody recognises is
    // dropped, so one typo does not silence the rest.
    if let Some(list) = m.get("kinds").and_then(|v| v.as_array()) {
        c.motivation_kinds =
            list.iter().filter_map(|v| v.as_str()).filter_map(crate::motivation::Kind::read).map(|k| k.word().to_string()).collect();
    }
}

fn apply_ai(c: &mut Config, ai: &Toml) {
    if let Some(v) = ai.get("share_terminal_context").and_then(|v| v.as_bool()) {
        c.ai_share_terminal_context = v;
    }
    if let Some(v) = ai.get("memory").and_then(|v| v.as_bool()) {
        c.ai_memory = v;
    }
    if let Some(v) = ai.get("show_reasoning").and_then(|v| v.as_bool()) {
        c.ai_show_reasoning = v;
    }
    // `[ai] budget` — a positive, finite USD soft-cap; anything else clears it.
    if let Some(v) = ai.get("budget").and_then(|v| v.as_num()) {
        c.ai_budget = (v.is_finite() && v > 0.0).then_some(v);
    }
    if let Some(v) = ai.get("network").and_then(|v| v.as_bool()) {
        c.ai_network = v;
    }
    // `[ai] context_window` — a positive token count overrides the model's own;
    // 0 or nonsense means "trust the model file", which is the default.
    if let Some(v) = ai.get("context_window").and_then(|v| v.as_int()) {
        c.ai_context_window = u32::try_from(v).unwrap_or(0);
    }
    // `[ai] compact_at` — the fraction of the window that triggers compaction.
    // Out-of-range values fall back to the default rather than producing a
    // harness that either never compacts or compacts on every turn.
    if let Some(v) = ai.get("compact_at").and_then(|v| v.as_num()) {
        let v = v as f32;
        c.ai_compact_at = if v.is_finite() && (0.1..=0.95).contains(&v) { v } else { crate::ai::budget::DEFAULT_COMPACT_AT };
    }
    // `[ai] mode = "manual" | "auto"` for shell `@ai` suggestions; anything
    // else falls back to the safe default.
    if let Some(v) = ai.get("mode").and_then(|v| v.as_str()) {
        c.ai_command_mode = if v.eq_ignore_ascii_case("auto") { "auto".into() } else { "manual".into() };
    }
    // `[ai.balance] strategy = "weighted|round_robin|cost|failover"`.
    if let Some(strat) = ai.get("balance").and_then(|b| b.get("strategy")).and_then(|v| v.as_str()) {
        c.ai_strategy = strat.to_string();
    }
    // `[[ai.model]]` tables — the primary-model pool. Each carries `id`
    // (optionally qualified by `provider`), a `weight`, and per-model
    // sampling overrides.
    if let Some(models) = ai.get("model").and_then(|v| v.as_array()) {
        // A document that declares any model REPLACES the pool (so a profile overlay
        // overrides rather than merges with the inherited global pool).
        c.ai_pool.clear();
        for m in models {
            let Some(id) = m.get("id").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()) else {
                continue;
            };
            warn_swallowed_ai_keys(id, m);
            let posu32 = |k: &str| m.get(k).and_then(|v| v.as_int()).filter(|n| *n > 0).map(|n| n as u32);
            let unit = |k: &str| m.get(k).and_then(|v| v.as_num()).map(|n| (n as f32).clamp(0.0, 1.0));
            c.ai_pool.push(AiModelSpec {
                id: id.trim().to_string(),
                provider: m.get("provider").and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
                api_key: m.get("api_key").and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
                weight: m.get("weight").and_then(|v| v.as_int()).filter(|n| *n >= 0).map(|n| n as u32).unwrap_or(DEFAULT_WEIGHT),
                temperature: unit("temperature"),
                top_p: unit("top_p"),
                top_k: posu32("top_k"),
                max_tokens: m.get("max_tokens").and_then(|v| v.as_int()).map(|n| n.clamp(1, 200_000) as u32),
                context_window: m.get("context_window").and_then(|v| v.as_int()).map(|n| n.clamp(1, 10_000_000) as u32),
                thinking: m.get("thinking").and_then(|v| v.as_bool()),
            });
        }
    }
}

fn apply_gates(c: &mut Config, g: &Toml) {
    if let Some(v) = g.get("enabled").and_then(|v| v.as_bool()) {
        c.gates_enabled = v;
    }
    if let Some(v) = g.get("require_pairing").and_then(|v| v.as_bool()) {
        c.gates_require_pairing = v;
    }
    // Unknown values fall back to the SAFE side of each switch, never the
    // permissive one: a typo must not silently start running chat messages.
    if let Some(v) = g.get("plain_text").and_then(|v| v.as_str()) {
        c.gates_plain_text = if v.eq_ignore_ascii_case("ignore") { "ignore".into() } else { "run".into() };
    }
    if let Some(v) = g.get("screenshot").and_then(|v| v.as_str()) {
        c.gates_screenshot = if v.eq_ignore_ascii_case("photo") { "photo".into() } else { "document".into() };
    }
    if let Some(v) = g.get("max_reply_messages").and_then(|v| v.as_int()) {
        c.gates_max_reply_messages = v.clamp(1, 20) as usize;
    }
    if let Some(v) = g.get("idle_timeout_minutes").and_then(|v| v.as_int()) {
        c.gates_idle_minutes = v.clamp(0, 43_200) as u64;
    }
    if let Some(v) = g.get("attach").and_then(|v| v.as_bool()) {
        c.gates_attach = v;
    }
    // Every OTHER sub-table is a channel: `[gates.telegram]` → a GateSpec named
    // "telegram". Keeping this generic is what lets a new adapter ship without
    // touching the config parser at all. A document declaring any gate REPLACES
    // the list, matching `[[ai.model]]` overlay semantics.
    if let Some(entries) = g.as_table() {
        let mut found = Vec::new();
        for (name, val) in entries {
            let Some(fields) = val.as_table() else { continue };
            let get = |k: &str| {
                fields.iter().find(|(n, _)| n == k).and_then(|(_, v)| v.as_str()).map(|s| s.trim().to_string())
            };
            found.push(GateSpec {
                channel: name.trim().to_ascii_lowercase(),
                token: get("token").unwrap_or_default(),
                allow: fields
                    .iter()
                    .find(|(n, _)| n == "allow")
                    .and_then(|(_, v)| v.as_array())
                    .map(|a| a.iter().filter_map(chat_id).collect())
                    .unwrap_or_default(),
            });
        }
        if !found.is_empty() {
            c.gates = found;
        }
    }
}

fn apply_plugins(c: &mut Config, p: &Toml) {
    if let Some(v) = p.get("enabled").and_then(|v| v.as_bool()) {
        c.plugins_enabled = v;
    }
    if let Some(arr) = p.get("disabled").and_then(|v| v.as_array()) {
        c.plugins_disabled = arr.iter().filter_map(|v| v.as_str().map(String::from)).collect();
    }
}

fn apply_shell(c: &mut Config, s: &Toml) {
    if let Some(v) = s.get("integration").and_then(|v| v.as_bool()) {
        c.shell_integration = v;
    }
}

fn apply_registry(c: &mut Config, r: &Toml) {
    if let Some(d) = r.get("dir").and_then(|v| v.as_str()) {
        c.registry_dir = d.to_string();
    }
}

fn apply_logging(c: &mut Config, lg: &Toml) {
    if let Some(v) = lg.get("level").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()) {
        c.log_level = v.trim().to_string();
    }
    if let Some(v) = lg.get("retention_days").and_then(|v| v.as_int()) {
        c.log_retention_days = v.max(0) as usize;
    }
}

fn apply_security(c: &mut Config, sec: &Toml) {
    let strs = |v: &Toml| {
        v.as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default()
    };
    if let Some(v) = sec.get("allowed_commands") {
        c.allowed_commands = strs(v);
    }
    if let Some(v) = sec.get("denied_commands") {
        c.denied_commands = strs(v);
    }
    if let Some(v) = sec.get("confirm_commands") {
        c.confirm_commands = strs(v);
    }
    if let Some(v) = sec.get("auto_safe_commands") {
        c.auto_safe_commands = strs(v);
    }
}

fn apply_redact(c: &mut Config, reds: &Toml) {
let Some(reds) = reds.as_array() else { return };
    for r in reds {
        if let Some(pattern) = r.get("pattern").and_then(|v| v.as_str()) {
            c.redactions.push(Redaction {
                pattern: pattern.to_string(),
                replacement: r.get("replacement").and_then(|v| v.as_str()).unwrap_or("\u{ab}redacted\u{bb}").to_string(),
                scope: r.get("scope").and_then(|v| v.as_str()).unwrap_or("all").to_string(),
                literal: r.get("literal").and_then(|v| v.as_bool()).unwrap_or(false),
            });
        }
    }
}

fn apply_keybinding(c: &mut Config, kbs: &Toml) {
let Some(kbs) = kbs.as_array() else { return };
    for k in kbs {
        if let (Some(key), Some(action)) =
            (k.get("key").and_then(|v| v.as_str()), k.get("action").and_then(|v| v.as_str()))
        {
            c.keybindings.push((key.to_string(), action.to_string()));
        }
    }
}

/// Keys that only ever belong to `[ai]` — a model table has no use for any of them.
/// Finding one inside a `[[ai.model]]` means the user wrote their model table ABOVE
/// their `[ai]` settings, and TOML handed those settings to the model instead.
const AI_ONLY_KEYS: [&str; 5] = ["share_terminal_context", "memory", "mode", "network", "show_reasoning"];

/// Warn when a `[[ai.model]]` table has swallowed `[ai]` settings. This is silent data
/// loss otherwise: the settings never reach `[ai]`, and a stray `api_key = ""` written
/// after the model overwrites the key the user set on it — the "AI key missing" report.
fn warn_swallowed_ai_keys(id: &str, m: &Toml) {
    let stolen: Vec<&str> = AI_ONLY_KEYS.into_iter().filter(|k| m.get(k).is_some()).collect();
    if stolen.is_empty() {
        return;
    }
    eprintln!(
        "aiTerminal: [[ai.model]] '{id}' contains [ai] settings ({}) — in TOML every key \
         after a table header joins THAT table, so these never reached [ai] (and an \
         `api_key = \"\"` written below the model wipes the key you set on it). \
         Fix: move every [[ai.model]] block BELOW all the plain [ai] settings.",
        stolen.join(", "),
    );
}
