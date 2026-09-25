//! RL phase 1: every `mini --move` run is an episode with a code-computed
//! reward, stored per host as JSONL, and a human label (`--feedback`).
//! Contract: plans/004-episode-log-reward-feedback.md.
//! STUB: bodies return safe defaults; worker `ep-core` implements them.
#![allow(dead_code, unused_variables)]

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Feedback {
    pub label: String,
    pub reason: Option<String>,
    pub ts_ms: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct StepSample {
    pub step: usize,
    pub operation: String,
    pub target: Option<String>,
    pub page_sig: String,
    pub url: String,
    pub page_changed: bool,
    pub goal_before: Option<f64>,
    pub goal_after: Option<f64>,
    pub ms: u64,
    pub reward: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Episode {
    pub id: String,
    pub ts_ms: u64,
    pub host: String,
    pub goal: String,
    pub goal_key: String,
    pub ok: bool,
    pub stop: String,
    pub judge_calls: u32,
    pub elapsed_ms: u64,
    pub steps: Vec<StepSample>,
    pub episode_reward: f64,
    pub feedback: Option<Feedback>,
}

impl Episode {
    pub fn final_reward(&self) -> f64 {
        self.episode_reward
    }
}

/// One step as `run_loop` saw it, before rewards are filled in.
#[derive(Clone, Debug, Default)]
pub struct RawStep {
    pub step: usize,
    pub operation: String,
    pub target: Option<String>,
    pub page_sig: String,
    pub url: String,
    pub page_changed: bool,
    pub goal_met: Option<f64>,
    pub ms: u64,
}

pub fn root_dir(dir: Option<&Path>) -> PathBuf {
    match dir {
        Some(d) => d.to_path_buf(),
        None => dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("Mini").join("episodes"),
    }
}

pub fn page_signature(url: &str, labels: &[String]) -> String {
    String::new()
}

pub fn normalize_goal(goal: &str) -> String {
    goal.to_string()
}

pub fn step_reward(goal_before: Option<f64>, goal_after: Option<f64>, page_changed: bool) -> f64 {
    0.0
}

pub fn episode_reward(ok: bool, steps: &[StepSample]) -> f64 {
    0.0
}

pub fn build_episode(goal: &str, ok: bool, stop: &str, judge_calls: u32, elapsed_ms: u64, raw: &[RawStep]) -> Episode {
    Episode { goal: goal.to_string(), ok, stop: stop.to_string(), judge_calls, elapsed_ms, ..Default::default() }
}

pub fn append(dir: Option<&Path>, ep: &Episode) -> Result<PathBuf, String> {
    Err("episodes::append is a stub".into())
}

pub fn set_feedback(dir: Option<&Path>, id: Option<&str>, label: &str, reason: Option<&str>) -> Result<Episode, String> {
    Err("episodes::set_feedback is a stub".into())
}

pub fn summary(dir: Option<&Path>, host: Option<&str>) -> Result<serde_json::Value, String> {
    Err("episodes::summary is a stub".into())
}
