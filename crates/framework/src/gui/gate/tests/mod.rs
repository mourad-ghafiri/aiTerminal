use super::*;
use std::sync::mpsc::{channel, Receiver};

const QUESTION: &str = "open ~/project in workspace mode?\n  this project would add: 2 agent(s) \u{b7} 1 MCP server(s) \u{2014} these run code as you";

fn open() -> (Gate, Receiver<bool>) {
    let (reply, answer) = channel();
    let mut g = Gate::new();
    g.open(QUESTION, reply);
    (g, answer)
}

#[test]
fn the_question_splits_into_a_title_and_its_inject_lines() {
    let (mut g, _answer) = open();
    let s = g.state_mut().unwrap();
    assert_eq!(s.title, "open ~/project in workspace mode?");
    assert_eq!(s.detail.len(), 1);
    assert!(s.detail[0].contains("MCP server"), "the injects survive whole: {:?}", s.detail);
}

#[test]
fn enter_on_a_fresh_gate_keeps_global_config() {
    // THE test, the confirm modal's rule applied here: trusting a folder executes
    // code, so the reflex keystroke must land on the safe answer.
    let (mut g, answer) = open();
    assert!(g.is_open());
    assert!(g.answer_focused());
    assert_eq!(answer.recv(), Ok(false), "Enter with the safe button focused declines");
    assert!(!g.is_open(), "and the modal closes — a decision was made");
}

#[test]
fn moving_focus_then_entering_grants_trust() {
    let (mut g, answer) = open();
    g.move_focus();
    assert!(g.answer_focused());
    assert_eq!(answer.recv(), Ok(true));

    // Focus flips back and forth — ←/→/Tab all land on the same two buttons.
    let (mut g, answer) = open();
    g.move_focus();
    g.move_focus();
    assert!(g.answer_focused());
    assert_eq!(answer.recv(), Ok(false), "back on the safe button");
}

#[test]
fn esc_declines_and_a_closed_gate_answers_nothing_twice() {
    let (mut g, answer) = open();
    assert!(g.decline());
    assert_eq!(answer.recv(), Ok(false));
    assert!(!g.is_open());
    assert!(!g.answer_focused(), "a decided gate has nothing left to answer");
    assert!(!g.decline());
}

#[test]
fn a_click_hits_the_button_under_it_and_the_backdrop_declines() {
    let (mut g, answer) = open();
    // The renderer records the rects; fake them so the hit-testing is what is tested.
    let s = g.state_mut().unwrap();
    s.button_rects = vec![
        (Button::Cancel, Rect::new(100.0, 100.0, 80.0, 30.0)),
        (Button::Confirm, Rect::new(200.0, 100.0, 80.0, 30.0)),
    ];
    assert!(g.click_at(Point::new(240.0, 110.0)), "on the open button");
    assert_eq!(answer.recv(), Ok(true));

    let (mut g, answer) = open();
    assert!(g.click_at(Point::new(10.0, 10.0)), "the backdrop decides too");
    assert_eq!(answer.recv(), Ok(false), "…and it decides safely");
}
