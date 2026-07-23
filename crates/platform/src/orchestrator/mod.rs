//! `platform-orchestrator` — the generic sequence/chain executor.
//!
//! Runs a list of items through a caller-supplied step function, in order. When
//! `chain` is true, each step's *textual contribution* is appended to a running
//! context string that later steps see — so step N can build on the output of
//! steps 1..N. This is the AI-agnostic core of multi-agent orchestration: the AI
//! layer supplies a `run_one` that performs one Claude agent run and returns its
//! answer as the contribution, but nothing here knows about agents or Claude.
#![forbid(unsafe_code)]

/// Run `items` in sequence. For each item, `run_one(item, &context)` produces a
/// result `R` plus a string *contribution*; when `chain` is true that
/// contribution is appended to `context` before the next item runs. `context`
/// starts as `base_context`. Returns one `R` per item, in order.
pub fn run_sequence<I, R, F>(items: &[I], base_context: &str, chain: bool, mut run_one: F) -> Vec<R>
where
    F: FnMut(&I, &str) -> (R, String),
{
    let mut context = base_context.to_string();
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let (result, contribution) = run_one(item, &context);
        if chain {
            context.push_str(&contribution);
        }
        out.push(result);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_each_item_in_order() {
        let items = ["a", "b", "c"];
        let seen = std::cell::RefCell::new(Vec::new());
        let out = run_sequence(&items, "", false, |it, _ctx| {
            seen.borrow_mut().push(*it);
            (it.to_uppercase(), String::new())
        });
        assert_eq!(out, vec!["A", "B", "C"]);
        assert_eq!(*seen.borrow(), vec!["a", "b", "c"]);
    }

    #[test]
    fn chain_threads_context_forward() {
        let items = [1u32, 2, 3];
        // Each step sees the concatenation of prior contributions.
        let out = run_sequence(&items, "seed", true, |n, ctx| {
            let saw = ctx.to_string();
            (saw, format!("|{n}"))
        });
        assert_eq!(out, vec!["seed", "seed|1", "seed|1|2"]);
    }

    #[test]
    fn no_chain_keeps_base_context_constant() {
        let items = [1u32, 2];
        let out = run_sequence(&items, "base", false, |_n, ctx| (ctx.to_string(), "x".to_string()));
        assert_eq!(out, vec!["base", "base"]);
    }

    #[test]
    fn empty_items_yields_empty() {
        let out: Vec<u8> = run_sequence::<u8, u8, _>(&[], "ctx", true, |_i, _c| (0, String::new()));
        assert!(out.is_empty());
    }
}
