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
fn order_is_chosen_first_then_failover_chain() {
    // Every strategy now yields a full failover chain: the strategy's pick first,
    // then the rest — so a run survives the head model dying (before any token).
    let p = ModelPool { entries: vec![entry("a", 1), entry("b", 1)], strategy: Strategy::Cost };
    let order: Vec<String> = p.order().into_iter().map(|m| m.id).collect();
    assert_eq!(order.len(), p.entries.len(), "no model dropped from the chain");
    assert_eq!(order[0], p.choose().id, "the strategy's pick leads the chain");
    // No duplicates: the head is not repeated among the fallbacks.
    let mut sorted = order.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), order.len(), "chain has no duplicate models");
}

#[test]
fn order_single_entry_is_just_that_model() {
    let p = ModelPool { entries: vec![entry("solo", 1)], strategy: Strategy::Weighted };
    assert_eq!(p.order().into_iter().map(|m| m.id).collect::<Vec<_>>(), vec!["solo".to_string()]);
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
    let model = cat.resolve("claude-opus-4-8");
    let on = PoolEntry::new(model.clone(), 1, ModelOverrides { thinking: Some(true), ..Default::default() });
    assert!(on.resolved().caps.enable_thinking, "thinking = true forces it on");
    let off = PoolEntry::new(model, 1, ModelOverrides { thinking: Some(false), ..Default::default() });
    assert!(!off.resolved().caps.enable_thinking, "thinking = false forces it off");
}
