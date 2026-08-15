use std::sync::Arc;

use super::*;

fn ctx_with(asker: Arc<dyn Asker>) -> CapCtx {
    CapCtx {
        guard: Arc::new(crate::guard::Guard::rooted("", crate::guard::Base::here())),
        app_data: None,
        remote_enabled: false,
        origin: String::new(),
        sandbox: None,
        memory_dir: None,
        approver: Arc::new(crate::guard::NobodyToAsk),
        asker,
    }
}

struct Scripted(&'static str);
impl Asker for Scripted {
    fn ask(&self, question: &str) -> Option<String> {
        Some(format!("{} (you asked: {question})", self.0))
    }
}

struct Declines;
impl Asker for Declines {
    fn ask(&self, _q: &str) -> Option<String> {
        None
    }
}

#[test]
fn a_question_reaches_the_human_and_their_words_come_back() {
    let ctx = ctx_with(Arc::new(Scripted("use the staging bucket")));
    let out = crate::caps::run("ask.user", &[("question".into(), "which bucket?".into())], &ctx).expect("answered");
    assert_eq!(out.as_str().unwrap(), "use the staging bucket (you asked: which bucket?)");
}

#[test]
fn a_decline_and_a_headless_run_refuse_in_the_same_words() {
    let declined = crate::caps::run("ask.user", &[("question".into(), "sure?".into())], &ctx_with(Arc::new(Declines))).unwrap_err();
    let headless = crate::caps::run("ask.user", &[("question".into(), "sure?".into())], &ctx_with(Arc::new(NobodyToAnswer))).unwrap_err();
    assert_eq!(declined, headless);
    assert!(declined.contains("nobody answered"));
}

#[test]
fn an_empty_question_is_refused_before_anyone_is_bothered() {
    let err = crate::caps::run("ask.user", &[("question".into(), "  ".into())], &ctx_with(Arc::new(Scripted("x")))).unwrap_err();
    assert!(err.contains("needs a question"));
}
