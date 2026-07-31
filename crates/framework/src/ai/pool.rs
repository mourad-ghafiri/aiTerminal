//! The model **pool** — multi-model selection + load balancing for the AI engine.
//!
//! A user declares several models in `config.toml` (`[[ai.model]]` tables), each with
//! a `weight` and optional per-model overrides, plus a `[ai.balance] strategy`. The
//! pool turns that into a runtime [`ModelPool`] of self-describing [`ModelDef`]s and
//! a [`Strategy`] that decides which model serves the next request:
//!
//! - **Weighted** (default): random pick proportional to each entry's weight, so a
//!   small weight (e.g. 10) makes an expensive model rare.
//! - **RoundRobin**: cycle through the entries in order, one per request.
//! - **Cost**: always the cheapest entry (by `price_in + price_out`).
//! - **Failover**: the first entry, with the ordered remainder as fallbacks (the
//!   collected agent path retries the next on a hard error — see `ai::agent`).
//!
//! Selection is **instant** (pure CPU, no network) so it is safe to call on the UI
//! thread before a streaming request. The RNG + round-robin cursor live in module
//! state (not in [`ModelPool`]), so the pool stays a plain `Clone + PartialEq` value
//! and the engine keeps zero external crates.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::OnceLock;

use corelib::wire::Json;

use crate::ai::provider::ModelDef;

/// How the pool picks the model for the next request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Strategy {
    /// Random pick proportional to each entry's weight (the default).
    #[default]
    Weighted,
    /// Cycle through the entries in declaration order, one per request.
    RoundRobin,
    /// The cheapest entry by `price_in + price_out`.
    Cost,
    /// The first entry; the ordered remainder are fallbacks for the agent path.
    Failover,
}

impl Strategy {
    /// Parse the `[ai.balance] strategy` value (unknown/empty → `Weighted`).
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "round_robin" | "roundrobin" | "rr" => Strategy::RoundRobin,
            "cost" | "cheapest" => Strategy::Cost,
            "failover" | "fallback" => Strategy::Failover,
            _ => Strategy::Weighted,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Strategy::Weighted => "weighted",
            Strategy::RoundRobin => "round_robin",
            Strategy::Cost => "cost",
            Strategy::Failover => "failover",
        }
    }
}

/// Per-model overrides from a `[[ai.model]]` table, applied on top of the model's
/// own definition. Each is optional — an unset field keeps the model's value.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ModelOverrides {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub max_tokens: Option<u32>,
    /// Force extended thinking on/off for this model (overrides the catalog cap).
    pub thinking: Option<bool>,
    /// This model's real context window, when the catalog is wrong about THIS
    /// deployment — a local model served with a smaller window than its card claims.
    /// Per-entry, because a mixed pool can hold a 32k local model beside a 200k
    /// hosted one and a single global number would be wrong for one of them.
    pub context_window: Option<u32>,
}

impl ModelOverrides {
    /// `true` when no override is set (so an entry can skip cloning work).
    pub fn is_empty(&self) -> bool {
        self.temperature.is_none()
            && self.top_p.is_none()
            && self.top_k.is_none()
            && self.max_tokens.is_none()
            && self.thinking.is_none()
            && self.context_window.is_none()
    }

    /// Apply the set overrides onto `m` in place.
    pub fn apply(&self, m: &mut ModelDef) {
        if let Some(t) = self.temperature {
            m.temperature = Some(t);
        }
        if let Some(p) = self.top_p {
            m.top_p = Some(p);
        }
        if let Some(k) = self.top_k {
            m.top_k = Some(k);
        }
        if let Some(mt) = self.max_tokens {
            m.max_tokens = mt;
        }
        if let Some(cw) = self.context_window {
            m.context_window = cw;
        }
        if let Some(th) = self.thinking {
            m.caps.enable_thinking = th;
        }
    }
}

/// One pool member: a resolved model, its load-balancing `weight`, and the
/// per-entry overrides to fold in when it is chosen.
#[derive(Clone, Debug, PartialEq)]
pub struct PoolEntry {
    pub model: ModelDef,
    pub weight: u32,
    pub overrides: ModelOverrides,
}

impl PoolEntry {
    pub fn new(model: ModelDef, weight: u32, overrides: ModelOverrides) -> Self {
        PoolEntry { model, weight, overrides }
    }

    /// The model with this entry's overrides applied (what the client actually uses).
    pub fn resolved(&self) -> ModelDef {
        if self.overrides.is_empty() {
            return self.model.clone();
        }
        let mut m = self.model.clone();
        self.overrides.apply(&mut m);
        m
    }

    fn price_sum(&self) -> f64 {
        self.model.pricing.price_in + self.model.pricing.price_out
    }
}

/// A set of candidate models + the strategy that picks among them. Plain value
/// data (`Clone + PartialEq`); the selection cursor/RNG live in module state.
/// `Default` is the EMPTY pool — AI is off until a model is declared.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelPool {
    pub entries: Vec<PoolEntry>,
    pub strategy: Strategy,
}

impl ModelPool {
    /// A one-entry pool of `model` (weight 1, no overrides) — the zero-config /
    /// pinned-model case. Selection always returns `model`.
    pub fn single(model: ModelDef) -> Self {
        ModelPool { entries: vec![PoolEntry::new(model, 1, ModelOverrides::default())], strategy: Strategy::Weighted }
    }

    /// The model to serve the next request, per the strategy. Never panics: an empty
    /// pool yields the builtin default model.
    pub fn choose(&self) -> ModelDef {
        match self.entries.len() {
            0 => return ModelDef::default(),
            1 => return self.entries[0].resolved(),
            _ => {}
        }
        match self.strategy {
            Strategy::Failover => self.entries[0].resolved(),
            Strategy::RoundRobin => {
                let n = self.entries.len();
                let i = next_round_robin() % n;
                self.entries[i].resolved()
            }
            Strategy::Cost => self
                .entries
                .iter()
                .min_by(|a, b| a.price_sum().partial_cmp(&b.price_sum()).unwrap_or(std::cmp::Ordering::Equal))
                .map(PoolEntry::resolved)
                .unwrap_or_default(),
            Strategy::Weighted => self.weighted_pick(),
        }
    }

    /// The ordered candidate list for a run: the strategy's [`choose`](Self::choose)
    /// result **first**, then every OTHER entry as a failover chain. This makes a run
    /// resilient under EVERY strategy — a weighted/round-robin/cost pick that dies
    /// (before it streams a token) falls over to a healthy pool member instead of
    /// killing the run. The head is picked once, so a caller that iterates turns on
    /// this list keeps ONE model for the whole run (no mid-run model hopping).
    pub fn order(&self) -> Vec<ModelDef> {
        if self.entries.len() <= 1 {
            return vec![self.choose()];
        }
        let head = self.choose();
        let mut out = vec![head.clone()];
        for e in &self.entries {
            let m = e.resolved();
            if m.id != head.id || m.provider != head.provider {
                out.push(m);
            }
        }
        out
    }

    /// A representative member for status display (the highest-weight entry, ties
    /// broken by declaration order) — used when nothing has run yet.
    pub fn representative(&self) -> ModelDef {
        self.entries
            .iter()
            .max_by_key(|e| e.weight)
            .map(PoolEntry::resolved)
            .unwrap_or_default()
    }

    fn weighted_pick(&self) -> ModelDef {
        let total: u64 = self.entries.iter().map(|e| u64::from(e.weight.max(0))).sum();
        if total == 0 {
            return self.entries[0].resolved();
        }
        let mut r = next_rng() % total;
        for e in &self.entries {
            let w = u64::from(e.weight);
            if r < w {
                return e.resolved();
            }
            r -= w;
        }
        self.entries.last().map(PoolEntry::resolved).unwrap_or_default()
    }

    /// `[{id, provider, weight, price_in, price_out}]` + `strategy` for the inspector
    /// (`ai.pool`). The host adds the live `pinned` / `last_used` fields.
    pub fn to_json(&self) -> Json {
        Json::Obj(vec![
            ("strategy".into(), Json::Str(self.strategy.as_str().to_string())),
            (
                "entries".into(),
                Json::Arr(
                    self.entries
                        .iter()
                        .map(|e| {
                            Json::Obj(vec![
                                ("id".into(), Json::Str(e.model.id.clone())),
                                ("provider".into(), Json::Str(e.model.provider.clone())),
                                ("weight".into(), Json::Num(e.weight as f64)),
                                ("price_in".into(), Json::Num(e.model.pricing.price_in)),
                                ("price_out".into(), Json::Num(e.model.pricing.price_out)),
                            ])
                        })
                        .collect(),
                ),
            ),
        ])
    }
}

/// Next value from a tiny `xorshift64` PRNG, seeded once from the OS CSPRNG (zero
/// external crates). Shared, lock-free; randomness quality is ample for picking a
/// model by weight.
fn next_rng() -> u64 {
    static STATE: OnceLock<AtomicU64> = OnceLock::new();
    let state = STATE.get_or_init(|| {
        let mut b = [0u8; 8];
        let seed = if platform::os::random_bytes(&mut b) {
            u64::from_le_bytes(b)
        } else {
            0x9E37_79B9_7F4A_7C15
        };
        AtomicU64::new(seed | 1) // never zero (xorshift fixed point)
    });
    let mut x = state.load(Ordering::Relaxed);
    loop {
        let mut y = x;
        y ^= y << 13;
        y ^= y >> 7;
        y ^= y << 17;
        match state.compare_exchange_weak(x, y, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return y,
            Err(cur) => x = cur,
        }
    }
}

/// Monotonic round-robin cursor (process-global). A single counter is correct
/// because requests are serialized through the host; modulo the pool size gives
/// the next index.
fn next_round_robin() -> usize {
    static CURSOR: AtomicUsize = AtomicUsize::new(0);
    CURSOR.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests;
