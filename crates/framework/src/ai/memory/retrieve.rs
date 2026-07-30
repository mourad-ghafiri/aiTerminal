//! The retrieval strategy that ranks memories against a query — a from-scratch,
//! **model-agnostic** lexical ranker (no embeddings, no DB, no external crates).
//!
//! [`Bm25Retriever`] scores each memory with Okapi **BM25** over a tiny tokenizer,
//! then re-ranks by **salience** (importance) and **recency** (a gentle forgetting
//! curve, refreshed on recall) — so a frequently-relevant, recently-reinforced fact
//! beats a stale one of equal lexical match. [`Retriever`] is a trait (Strategy), so
//! a future ranker can drop in without touching callers.

use super::MemoryEntry;

/// Ranks memories for a query. Returns `(index, score)` pairs sorted by score
/// descending; only entries with a positive lexical match are included.
pub trait Retriever: Send + Sync {
    fn rank(&self, query: &str, entries: &[MemoryEntry], now: u64) -> Vec<(usize, f32)>;
}

/// Okapi BM25 + salience/recency re-rank (the default strategy).
#[derive(Clone, Copy, Debug)]
pub struct Bm25Retriever {
    pub k1: f32,
    pub b: f32,
    /// Weight of an entry's salience in the final score (`score·(1 + w·salience)`).
    pub salience_weight: f32,
    /// Per-day decay of the recency factor since the entry was last touched/recalled.
    pub recency_decay: f32,
    /// Boost per query term that exactly matches one of the entry's TAGS.
    ///
    /// A tag is a deliberate act — somebody decided this note is about `release` —
    /// while the same word in a body may be an aside. BM25 cannot tell the two apart,
    /// because `searchable()` hands it one flat bag of words. This is where that
    /// distinction is put back.
    pub tag_boost: f32,
}

impl Default for Bm25Retriever {
    fn default() -> Self {
        Bm25Retriever { k1: 1.2, b: 0.75, salience_weight: 0.5, recency_decay: 0.03, tag_boost: 0.35 }
    }
}

impl Retriever for Bm25Retriever {
    fn rank(&self, query: &str, entries: &[MemoryEntry], now: u64) -> Vec<(usize, f32)> {
        let q = tokenize(query);
        if q.is_empty() || entries.is_empty() {
            return Vec::new();
        }
        // Index: per-doc token lists + corpus document-frequency per term.
        let docs: Vec<Vec<String>> = entries.iter().map(|e| tokenize(&e.searchable())).collect();
        let n = docs.len() as f32;
        let avgdl = (docs.iter().map(|d| d.len()).sum::<usize>() as f32 / n).max(1.0);
        let mut df: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
        for d in &docs {
            let mut seen = std::collections::HashSet::new();
            for t in d {
                if seen.insert(t.as_str()) {
                    *df.entry(t.as_str()).or_insert(0) += 1;
                }
            }
        }
        let q_uniq: std::collections::HashSet<&str> = q.iter().map(String::as_str).collect();

        let mut scored: Vec<(usize, f32)> = Vec::new();
        for (i, d) in docs.iter().enumerate() {
            let dl = d.len() as f32;
            let mut bm = 0.0_f32;
            for term in &q_uniq {
                let tf = d.iter().filter(|t| t.as_str() == *term).count() as f32;
                if tf == 0.0 {
                    continue;
                }
                let dfi = *df.get(*term).unwrap_or(&0) as f32;
                let idf = (((n - dfi + 0.5) / (dfi + 0.5)) + 1.0).ln();
                bm += idf * (tf * (self.k1 + 1.0)) / (tf + self.k1 * (1.0 - self.b + self.b * dl / avgdl));
            }
            if bm <= 0.0 {
                continue;
            }
            let e = &entries[i];
            // An exact tag hit is worth more than the same word appearing in prose.
            let tag_hits = e
                .tags
                .iter()
                .filter(|t| {
                    let t = t.trim().to_lowercase();
                    !t.is_empty() && q_uniq.contains(t.as_str())
                })
                .count() as f32;
            let factor = (1.0 + self.salience_weight * e.salience.max(0.0))
                * (1.0 + self.tag_boost * tag_hits)
                * recency(e.updated, now, self.recency_decay);
            scored.push((i, bm * factor));
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored
    }
}

/// The recency multiplier: `1 / (1 + decay · age_in_days)` — recent (or recently
/// recalled, since `updated` is bumped on reinforcement) memories rank higher.
fn recency(updated: u64, now: u64, decay: f32) -> f32 {
    let age_days = now.saturating_sub(updated) as f32 / 86_400.0;
    1.0 / (1.0 + decay * age_days)
}

/// A minimal tokenizer: lowercase, split on non-alphanumeric, drop stopwords and
/// 1-char tokens, then fold each word to its stem. Good enough for BM25 over short
/// project notes; zero dependencies.
pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2)
        .map(|w| w.to_lowercase())
        .filter(|w| !is_stopword(w))
        .map(|w| stem(&w))
        .collect()
}

/// Fold the endings that make one word look like two.
///
/// Without this, a note saying "database **migrations** run with sqlx migrate" was
/// invisible to "how do I apply a **migration**" — the ranker is lexical, the two words
/// share no token, and the memory simply did not come back. Every existing test happened
/// to use the same word form on both sides, so nothing caught it.
///
/// Both the index and the query go through [`tokenize`], so the two can never disagree
/// about a stem. And because identical inputs stem identically, this can only ever MERGE
/// word forms — a pair that matched before still matches.
fn stem(w: &str) -> String {
    let n = w.len();
    // Digits and identifiers are names, not English: `v2`, `eu-west-1`, `sqlx`.
    if w.chars().any(|c| c.is_ascii_digit()) {
        return w.to_string();
    }
    let cut = |suffix: &str, least: usize| (n >= least + suffix.len() && w.ends_with(suffix)).then(|| n - suffix.len());
    // `retries` → `retry`, so the plural and the singular land on one token.
    if n >= 5 && w.ends_with("ies") {
        return format!("{}y", &w[..n - 3]);
    }
    // `-ss` and `-us` are not plurals (`class`, `status`), and neither is a two-letter word.
    if let Some(at) = cut("es", 3).filter(|_| w.ends_with("ches") || w.ends_with("shes") || w.ends_with("xes") || w.ends_with("ses")) {
        return w[..at].to_string();
    }
    if n >= 4 && w.ends_with('s') && !w.ends_with("ss") && !w.ends_with("us") && !w.ends_with("is") {
        return w[..n - 1].to_string();
    }
    for suffix in ["ing", "ed"] {
        if let Some(at) = cut(suffix, 4) {
            return w[..at].to_string();
        }
    }
    w.to_string()
}

fn is_stopword(w: &str) -> bool {
    matches!(
        w,
        "the" | "and" | "for" | "are" | "was" | "with" | "you" | "your" | "that" | "this" | "from"
            | "have" | "has" | "had" | "not" | "but" | "all" | "can" | "will" | "into" | "out" | "use"
            | "via" | "its" | "they" | "them" | "then" | "than" | "when" | "what" | "which"
            | "who" | "how" | "why" | "where" | "our" | "their" | "his" | "her" | "she" | "him" | "may"
            | "any" | "get" | "got" | "let" | "one" | "two" | "per" | "etc"
            | "is" | "of" | "in" | "on" | "at" | "to" | "be" | "or" | "as" | "an" | "by" | "if" | "do"
            | "we" | "no" | "so" | "up" | "it" | "us"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::memory::MemoryEntry;

    fn mem(id: &str, body: &str) -> MemoryEntry {
        MemoryEntry::new(id.into(), "fact".into(), Vec::new(), body.into(), 1_000)
    }

    #[test]
    fn tokenizer_drops_stopwords_and_short_words() {
        let t = tokenize("The API base is /v2, auth via X-Token!");
        assert!(t.contains(&"api".to_string()));
        assert!(t.contains(&"base".to_string()));
        assert!(t.contains(&"token".to_string()));
        assert!(!t.contains(&"the".to_string()) && !t.contains(&"is".to_string()) && !t.contains(&"via".to_string()));
    }

    #[test]
    fn one_word_in_two_forms_is_one_token() {
        // The defect this closes: a note saying "migrations" was invisible to a question
        // asking about a "migration", because the ranker is lexical and the two share no
        // token. Every earlier test used the same form on both sides.
        let same = |a: &str, b: &str| assert_eq!(tokenize(a), tokenize(b), "{a:?} and {b:?} should be one token");
        same("migrations", "migration");
        same("deploys", "deploy");
        same("deployed", "deploy");
        same("deploying", "deploy");
        same("retries", "retry");
        same("branches", "branch");

        // What must NOT be folded: a word that only looks plural, and anything with a
        // digit in it — those are names (`v2`, `eu-west-1`), not English.
        assert_eq!(tokenize("class"), vec!["class"]);
        assert_eq!(tokenize("status"), vec!["status"]);
        assert_eq!(tokenize("analysis"), vec!["analysis"]);
        assert_eq!(tokenize("sqlx"), vec!["sqlx"]);
        assert_ne!(tokenize("logs"), tokenize("login"));
    }

    #[test]
    fn stemming_only_ever_merges_word_forms() {
        // The safety property behind the change: identical inputs stem identically, so a
        // pair that matched before still matches. Only new matches can appear.
        for w in ["api", "deploy", "status", "class", "eu", "v2", "sqlx", "migration", "retry"] {
            assert_eq!(tokenize(w), tokenize(w));
            assert!(!tokenize(w).is_empty() || w.len() < 2, "{w} vanished");
        }
    }

    #[test]
    fn a_question_finds_the_note_that_answers_it_in_another_form() {
        let entries = vec![
            mem("a", "Database migrations run with sqlx migrate run before the service starts"),
            mem("b", "The design review meeting is on Thursdays"),
        ];
        let ranked = Bm25Retriever::default().rank("how do I apply a migration", &entries, 1_000);
        assert_eq!(entries[ranked[0].0].id, "a", "{ranked:?}");
    }

    #[test]
    fn bm25_ranks_relevant_above_noise() {
        let entries = vec![
            mem("a", "Deploy runs on push to main; CI builds the release"),
            mem("b", "The cat sat on a warm windowsill in the afternoon sun"),
            mem("c", "Prod database is us-east-1; staging is eu-west"),
        ];
        let r = Bm25Retriever::default();
        let ranked = r.rank("how do i deploy to production", &entries, 1_000);
        assert!(!ranked.is_empty());
        // The deploy note must outrank the unrelated cat note.
        let top = ranked[0].0;
        assert_eq!(entries[top].id, "a", "deploy note ranks first: {ranked:?}");
        assert!(!ranked.iter().any(|(i, _)| entries[*i].id == "b"), "irrelevant note is not matched");
    }

    #[test]
    fn salience_breaks_lexical_ties() {
        let mut hi = mem("hi", "rate limit is 100 requests per minute");
        let lo = mem("lo", "rate limit is 100 requests per minute");
        hi.salience = 3.0;
        let r = Bm25Retriever::default();
        let ranked = r.rank("rate limit requests", &[lo, hi], 1_000);
        assert_eq!(ranked[0].0, 1, "the higher-salience duplicate ranks first");
    }

    #[test]
    fn recency_favours_recently_updated() {
        let mut old = mem("old", "use ripgrep for searching the codebase");
        let mut fresh = mem("fresh", "use ripgrep for searching the codebase");
        old.updated = 0; // ~ now/86400 days old
        fresh.updated = 1_000_000;
        let r = Bm25Retriever::default();
        let ranked = r.rank("search codebase ripgrep", &[old, fresh], 1_000_000);
        assert_eq!(ranked[0].0, 1, "the freshly-updated memory ranks first");
    }
}
