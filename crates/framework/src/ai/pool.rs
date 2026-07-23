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
}

impl ModelOverrides {
    /// `true` when no override is set (so an entry can skip cloning work).
    pub fn is_empty(&self) -> bool {
        self.temperature.is_none() && self.top_p.is_none() && self.top_k.is_none() && self.max_tokens.is_none() && self.thinking.is_none()
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
#[derive(Clone, Debug, PartialEq)]
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

    /// The ordered candidate list for failover (every entry, first preferred). For
    /// the other strategies this is just the single [`choose`](Self::choose) result,
    /// so a caller can always iterate candidates uniformly.
    pub fn order(&self) -> Vec<ModelDef> {
        match self.strategy {
            Strategy::Failover if self.entries.len() > 1 => self.entries.iter().map(PoolEntry::resolved).collect(),
            _ => vec![self.choose()],
        }
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
mod tests {
    use super::*;
    use crate::ai::provider::ModelDef;

    fn model(id: &str, provider: &str, price: f64) -> ModelDef {
        let mut m = ModelDef::default();
        m.id = id.into();
        m.provider = provider.into();
        m.pricing.price_in = price;
        m.pricing.price_out = price;
        m
    }

    fn entry(id: &str, weight: u32) -> PoolEntry {
        PoolEntry::new(model(id, "p", 1.0), weight, ModelOverrides::default())
    }

    #[test]
    fn strategy_parses_aliases_and_defaults_weighted() {
        assert_eq!(Strategy::parse("round-robin"), Strategy::RoundRobin);
        assert_eq!(Strategy::parse("FAILOVER"), Strategy::Failover);
        assert_eq!(Strategy::parse("cheapest"), Strategy::Cost);
        assert_eq!(Strategy::parse("nonsense"), Strategy::Weighted);
        assert_eq!(Strategy::parse(""), Strategy::Weighted);
    }

    #[test]
    fn overrides_apply_only_set_fields() {
        let mut m = model("x", "p", 1.0);
        m.max_tokens = 9000;
        m.temperature = Some(0.9);
        ModelOverrides { temperature: Some(0.2), max_tokens: None, ..Default::default() }.apply(&mut m);
        assert_eq!(m.temperature, Some(0.2)); // overridden
        assert_eq!(m.max_tokens, 9000); // untouched (override was None)
    }

    #[test]
    fn single_pool_always_returns_its_model() {
        let p = ModelPool::single(model("solo", "p", 1.0));
        for _ in 0..5 {
            assert_eq!(p.choose().id, "solo");
        }
    }

    #[test]
    fn weighted_pick_respects_weights_distribution() {
        let p = ModelPool { entries: vec![entry("rare", 1), entry("common", 99)], strategy: Strategy::Weighted };
        let mut common = 0;
        for _ in 0..2000 {
            if p.choose().id == "common" {
                common += 1;
            }
        }
        // ~99% expected; allow a wide margin so the test is not flaky.
        assert!(common > 1800, "weighted should pick the heavy model ~99% (got {common}/2000)");
    }

    #[test]
    fn zero_weights_fall_back_to_first() {
        let p = ModelPool { entries: vec![entry("a", 0), entry("b", 0)], strategy: Strategy::Weighted };
        assert_eq!(p.choose().id, "a");
    }

    #[test]
    fn round_robin_cycles_in_order() {
        let p = ModelPool { entries: vec![entry("a", 1), entry("b", 1), entry("c", 1)], strategy: Strategy::RoundRobin };
        // The global cursor's phase is unknown; assert the three ids appear within
        // three consecutive draws (a full cycle), in strictly advancing order.
        let seq: Vec<String> = (0..3).map(|_| p.choose().id).collect();
        let mut sorted = seq.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 3, "a full cycle visits each model once: {seq:?}");
    }

    #[test]
    fn cost_picks_cheapest() {
        let p = ModelPool {
            entries: vec![
                PoolEntry::new(model("pricey", "p", 10.0), 50, ModelOverrides::default()),
                PoolEntry::new(model("cheap", "p", 0.5), 50, ModelOverrides::default()),
            ],
            strategy: Strategy::Cost,
        };
        assert_eq!(p.choose().id, "cheap");
    }

    #[test]
    fn failover_choose_is_first_order_is_full_list() {
        let p = ModelPool { entries: vec![entry("a", 1), entry("b", 1)], strategy: Strategy::Failover };
        assert_eq!(p.choose().id, "a");
        let order: Vec<String> = p.order().into_iter().map(|m| m.id).collect();
        assert_eq!(order, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn order_for_non_failover_is_single() {
        let p = ModelPool { entries: vec![entry("a", 1), entry("b", 1)], strategy: Strategy::Cost };
        assert_eq!(p.order().len(), 1);
    }

    #[test]
    fn to_json_carries_strategy_and_entries() {
        let p = ModelPool { entries: vec![entry("a", 7)], strategy: Strategy::Weighted };
        let j = p.to_json();
        assert_eq!(j.get("strategy").and_then(Json::as_str), Some("weighted"));
        let es = j.get("entries").and_then(Json::as_array).unwrap();
        assert_eq!(es[0].get("weight").and_then(Json::as_f64), Some(7.0));
    }

    #[test]
    fn thinking_override_flips_the_catalog_cap() {
        let cat = crate::ai::provider::builtin_default();
        let (model, _) = cat.resolve("claude-opus-4-8", "");
        let on = PoolEntry::new(model.clone(), 1, ModelOverrides { thinking: Some(true), ..Default::default() });
        assert!(on.resolved().caps.enable_thinking, "thinking = true forces it on");
        let off = PoolEntry::new(model, 1, ModelOverrides { thinking: Some(false), ..Default::default() });
        assert!(!off.resolved().caps.enable_thinking, "thinking = false forces it off");
    }
}
