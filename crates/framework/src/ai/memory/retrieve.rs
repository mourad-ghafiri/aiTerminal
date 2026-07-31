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
mod tests;
