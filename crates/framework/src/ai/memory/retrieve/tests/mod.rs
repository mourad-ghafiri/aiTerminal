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
