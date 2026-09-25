//! Page affordances: search boxes, fake search triggers, label similarity,
//! and when a page really needs a scroll-through. Contract: plans/007.
//! STUB: worker p7-afford implements.
#![allow(dead_code, unused_variables)]

use crate::qa::ObservedElement;

pub fn is_search_like(el: &ObservedElement) -> bool {
    false
}

pub fn is_fake_search_trigger(el: &ObservedElement) -> bool {
    false
}

pub fn goal_wants_search(goal: &str) -> bool {
    false
}

pub fn label_similarity(a: &str, b: &str) -> f64 {
    if a == b { 1.0 } else { 0.0 }
}

pub fn needs_scroll_through(visible_text: &str, full_text: &str, goal_met: Option<f64>) -> bool {
    true
}
