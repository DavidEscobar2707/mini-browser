//! Jev client (POST https://api.typesafe.ai/v1/systemone) and typed answer
//! validation. Port of `jev_ultrafast/model.py`'s transport + `validate_choice`.
//!
//! Frozen signatures — implement the bodies, do not change the shapes.

use crate::qa::Judge;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;
use ureq::Agent;

#[derive(Clone, Debug)]
pub struct ChoiceAnswer {
    pub choice: String,
    pub confidence: f64,
    pub probabilities: BTreeMap<String, f64>,
}

#[derive(Clone, Debug)]
pub struct ScoreAnswer {
    pub score: f64,
    pub confidence: f64,
    pub probabilities: BTreeMap<String, f64>,
}

/// SPEC: TYPESAFE.md §2.
const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
/// Rate limited or momentarily overloaded — worth a retry (TYPESAFE.md §2 Errors).
const RETRYABLE_STATUS: [u16; 3] = [429, 529, 503];
/// Total attempts, including the first.
const MAX_ATTEMPTS: u32 = 3;

pub struct JevClient {
    api_key: String,
    model: String,
    agent: Agent,
    calls: AtomicU32,
    input_tokens: AtomicU64,
}

impl JevClient {
    /// Blocking client; 25 s per-call timeout, 3 attempts on 429/529/503.
    pub fn new(api_key: String, model: String) -> Self {
        let config = Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(25)))
            .build();
        JevClient {
            api_key,
            model,
            agent: Agent::new_with_config(config),
            calls: AtomicU32::new(0),
            input_tokens: AtomicU64::new(0),
        }
    }
}

impl Judge for JevClient {
    fn answers(&self, body: &serde_json::Value) -> Result<serde_json::Value, String> {
        let auth = format!("Bearer {}", self.api_key);
        // The client owns the model: a caller that forgets the field must not
        // silently get whatever the provider defaults to.
        let mut body = body.clone();
        if let Some(object) = body.as_object_mut() {
            object.insert("model".to_string(), serde_json::Value::String(self.model.clone()));
        }
        for attempt in 0..MAX_ATTEMPTS {
            match self.agent.post(ENDPOINT).header("Authorization", auth.as_str()).send_json(&body) {
                Ok(mut resp) => {
                    let parsed: serde_json::Value = resp
                        .body_mut()
                        .read_json()
                        .map_err(|e| format!("invalid JSON from model: {e}"))?;
                    let answers = parsed
                        .get("answers")
                        .cloned()
                        .ok_or_else(|| "model response missing 'answers'".to_string())?;
                    if let Some(tokens) =
                        parsed.get("usage").and_then(|u| u.get("input_tokens")).and_then(|v| v.as_u64())
                    {
                        self.input_tokens.fetch_add(tokens, Ordering::Relaxed);
                    }
                    self.calls.fetch_add(1, Ordering::Relaxed);
                    return Ok(answers);
                }
                // Retry rate-limit/overload responses with exponential backoff, unless
                // this was already the last attempt.
                Err(ureq::Error::StatusCode(code))
                    if RETRYABLE_STATUS.contains(&code) && attempt + 1 < MAX_ATTEMPTS =>
                {
                    std::thread::sleep(Duration::from_secs_f64(0.5 * 2f64.powi(attempt as i32)));
                }
                // Any other status (or a retryable one with no attempts left): the
                // status is safe to surface, nothing here can carry the key.
                Err(ureq::Error::StatusCode(code)) => {
                    return Err(format!("model provider returned HTTP {code}; no action executed"));
                }
                // Transport-level failure (DNS, connect, TLS, timeout, ...): never
                // echo the underlying error, it could embed request state.
                Err(_) => return Err("model connection failed".to_string()),
            }
        }
        Err("model unavailable".to_string())
    }

    fn calls(&self) -> u32 {
        self.calls.load(Ordering::Relaxed)
    }

    fn input_tokens(&self) -> u64 {
        self.input_tokens.load(Ordering::Relaxed)
    }
}

/// `choice` ∈ ids, probability keys == ids, all finite in [0,1], sum 1 ± 0.02,
/// the chosen option holds the max probability.
pub fn validate_choice(answer: &serde_json::Value, ids: &[String]) -> Result<ChoiceAnswer, String> {
    const INVALID: &str = "Invalid TypeSafe response; no action executed.";
    let invalid = || Err(INVALID.to_string());

    let choice = match answer.get("choice").and_then(|v| v.as_str()) {
        Some(c) => c.to_string(),
        None => return invalid(),
    };
    let confidence = match answer.get("confidence").and_then(|v| v.as_f64()) {
        Some(n) => n,
        None => return invalid(),
    };
    let raw_probabilities = match answer.get("probabilities").and_then(|v| v.as_object()) {
        Some(m) => m,
        None => return invalid(),
    };

    let mut probabilities = BTreeMap::new();
    for (key, value) in raw_probabilities {
        match value.as_f64() {
            Some(n) => {
                probabilities.insert(key.clone(), n);
            }
            None => return invalid(),
        }
    }

    let id_set: std::collections::BTreeSet<&str> = ids.iter().map(String::as_str).collect();
    let prob_keys: std::collections::BTreeSet<&str> = probabilities.keys().map(String::as_str).collect();
    let unit_range = |n: f64| n.is_finite() && (0.0..=1.0).contains(&n);
    let sum: f64 = probabilities.values().sum();
    let max_probability = probabilities.values().copied().fold(f64::NEG_INFINITY, f64::max);
    let chosen_probability = probabilities.get(choice.as_str()).copied().unwrap_or(f64::NEG_INFINITY);

    let valid = id_set.contains(choice.as_str())
        && prob_keys == id_set
        && unit_range(confidence)
        && probabilities.values().all(|&n| unit_range(n))
        && (sum - 1.0).abs() < 0.02
        && chosen_probability >= max_probability - 1e-6;

    if !valid {
        return invalid();
    }
    Ok(ChoiceAnswer { choice, confidence, probabilities })
}

/// `noul` is the probability of "yes".
pub fn read_noul(answer: &serde_json::Value) -> Result<f64, String> {
    match answer.get("noul").and_then(|v| v.as_f64()) {
        Some(n) if n.is_finite() && (0.0..=1.0).contains(&n) => Ok(n),
        _ => Err("Invalid TypeSafe noul response; no action executed.".to_string()),
    }
}

/// `score` = probability-weighted mean of level numbers.
pub fn read_score(answer: &serde_json::Value) -> Result<ScoreAnswer, String> {
    const INVALID: &str = "Invalid TypeSafe score response; no action executed.";
    let invalid = || Err(INVALID.to_string());
    let unit_range = |n: f64| n.is_finite() && (0.0..=1.0).contains(&n);

    let score = match answer.get("score").and_then(|v| v.as_f64()) {
        Some(n) if n.is_finite() => n,
        _ => return invalid(),
    };
    let confidence = match answer.get("confidence").and_then(|v| v.as_f64()) {
        Some(n) if unit_range(n) => n,
        _ => return invalid(),
    };
    let raw_probabilities = match answer.get("probabilities").and_then(|v| v.as_object()) {
        Some(m) if !m.is_empty() => m,
        _ => return invalid(),
    };

    let mut probabilities = BTreeMap::new();
    for (key, value) in raw_probabilities {
        match value.as_f64() {
            Some(n) if unit_range(n) => {
                probabilities.insert(key.clone(), n);
            }
            _ => return invalid(),
        }
    }
    let sum: f64 = probabilities.values().sum();
    if (sum - 1.0).abs() >= 0.02 {
        return invalid();
    }

    Ok(ScoreAnswer { score, confidence, probabilities })
}

/// Where `api_key()` looks after the environment, in order: Mini's own data
/// dir, then the jev-ultrafast checkout on the Desktop (the Python reference
/// client keeps its key there, so one key file serves both).
pub fn key_files() -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    if let Some(d) = dirs::data_local_dir() {
        files.push(d.join("Mini").join(".env"));
    }
    if let Some(d) = dirs::desktop_dir() {
        files.push(d.join("jev-ultrafast").join(".env"));
    }
    files
}

/// A Jev-related setting (`TYPESAFE_*`, `TEXT_MODEL_*`): the environment
/// first, else the first `key_files()` entry that defines it. Agents launch
/// Mini from fresh shells, so an exported variable alone is not enough to be
/// usable unattended.
pub fn setting(name: &str) -> Option<String> {
    if let Some(v) = std::env::var(name).ok().filter(|v| !v.trim().is_empty()) {
        return Some(v.trim().to_string());
    }
    key_files()
        .iter()
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .find_map(|text| dotenv_value(&text, name))
}

/// The Jev API key (`TYPESAFE_API_KEY`, see [`setting`]).
pub fn api_key() -> Option<String> {
    setting("TYPESAFE_API_KEY")
}

/// `TYPESAFE_MODEL`, else `jev-latest`.
pub fn model() -> String {
    setting("TYPESAFE_MODEL").unwrap_or_else(|| crate::qa::DEFAULT_MODEL.to_string())
}

/// Human-readable list of where the key may live, for "key missing" errors.
pub fn key_help() -> String {
    let files: Vec<String> = key_files().iter().map(|f| f.display().to_string()).collect();
    if files.is_empty() {
        return "set TYPESAFE_API_KEY in the environment".to_string();
    }
    format!("set TYPESAFE_API_KEY, or put TYPESAFE_API_KEY=... in one of: {}", files.join(", "))
}

/// Reads `KEY=value` from dotenv text: skips comments, accepts `export `,
/// strips matching quotes and a trailing CR. Empty values count as absent.
fn dotenv_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let (k, v) = line.split_once('=')?;
        if k.trim() != key {
            return None;
        }
        let v = v.trim();
        let v = v
            .strip_prefix('"')
            .and_then(|x| x.strip_suffix('"'))
            .or_else(|| v.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')))
            .unwrap_or(v)
            .trim();
        (!v.is_empty()).then(|| v.to_string())
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn dotenv_value_reads_the_key_in_common_dotenv_shapes() {
        let text = "# comment\r\nOTHER=1\r\nexport TYPESAFE_API_KEY=\"abc 123\"\r\n";
        assert_eq!(dotenv_value(text, "TYPESAFE_API_KEY").as_deref(), Some("abc 123"));
        assert_eq!(dotenv_value("TYPESAFE_API_KEY='k'", "TYPESAFE_API_KEY").as_deref(), Some("k"));
        assert_eq!(dotenv_value("TYPESAFE_API_KEY_OLD=x\nTYPESAFE_API_KEY=y", "TYPESAFE_API_KEY").as_deref(), Some("y"));
        assert_eq!(dotenv_value("TYPESAFE_API_KEY=", "TYPESAFE_API_KEY"), None);
        assert_eq!(dotenv_value("#TYPESAFE_API_KEY=z", "TYPESAFE_API_KEY"), None);
    }

    use super::*;
    use serde_json::json;

    fn ids(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn jev_client_starts_with_zero_usage() {
        let client = JevClient::new("secret-key".to_string(), "jev-latest".to_string());
        assert_eq!(client.model, "jev-latest");
        assert_eq!(client.calls(), 0);
        assert_eq!(client.input_tokens(), 0);
    }

    #[test]
    fn validate_choice_accepts_a_good_answer() {
        let answer = json!({
            "choice": "b",
            "confidence": 0.91,
            "probabilities": {"a": 0.1, "b": 0.7, "c": 0.2},
        });
        let out = validate_choice(&answer, &ids(&["a", "b", "c"])).expect("valid answer");
        assert_eq!(out.choice, "b");
        assert!((out.confidence - 0.91).abs() < 1e-9);
        assert!((out.probabilities["b"] - 0.7).abs() < 1e-9);
    }

    #[test]
    fn validate_choice_rejects_choice_outside_ids() {
        let answer = json!({
            "choice": "z",
            "confidence": 0.9,
            "probabilities": {"a": 0.5, "b": 0.5},
        });
        assert!(validate_choice(&answer, &ids(&["a", "b"])).is_err());
    }

    #[test]
    fn validate_choice_rejects_probability_key_mismatch() {
        let answer = json!({
            "choice": "a",
            "confidence": 0.9,
            "probabilities": {"a": 0.6, "c": 0.4},
        });
        assert!(validate_choice(&answer, &ids(&["a", "b"])).is_err());
    }

    #[test]
    fn validate_choice_rejects_out_of_range_probability() {
        let answer = json!({
            "choice": "a",
            "confidence": 0.9,
            "probabilities": {"a": 1.5, "b": -0.5},
        });
        assert!(validate_choice(&answer, &ids(&["a", "b"])).is_err());
    }

    #[test]
    fn validate_choice_rejects_distribution_far_from_one() {
        let answer = json!({
            "choice": "a",
            "confidence": 0.9,
            "probabilities": {"a": 0.3, "b": 0.3},
        });
        assert!(validate_choice(&answer, &ids(&["a", "b"])).is_err());
    }

    #[test]
    fn validate_choice_rejects_choice_not_holding_the_max() {
        let answer = json!({
            "choice": "a",
            "confidence": 0.9,
            "probabilities": {"a": 0.3, "b": 0.7},
        });
        assert!(validate_choice(&answer, &ids(&["a", "b"])).is_err());
    }

    #[test]
    fn validate_choice_rejects_non_numeric_probability() {
        let answer = json!({
            "choice": "a",
            "confidence": 0.9,
            "probabilities": {"a": "0.6", "b": 0.4},
        });
        assert!(validate_choice(&answer, &ids(&["a", "b"])).is_err());
    }

    #[test]
    fn validate_choice_rejects_missing_fields() {
        assert!(validate_choice(&json!({}), &ids(&["a", "b"])).is_err());
        assert!(validate_choice(&json!({"choice": "a"}), &ids(&["a", "b"])).is_err());
    }

    #[test]
    fn read_noul_parses_probability() {
        assert!((read_noul(&json!({"noul": 0.73})).unwrap() - 0.73).abs() < 1e-9);
    }

    #[test]
    fn read_noul_rejects_out_of_range_or_missing() {
        assert!(read_noul(&json!({"noul": 1.4})).is_err());
        assert!(read_noul(&json!({})).is_err());
    }

    #[test]
    fn read_score_parses_answer() {
        let answer = json!({
            "score": 1.4,
            "confidence": 0.8,
            "probabilities": {"0": 0.1, "1": 0.5, "2": 0.4},
        });
        let out = read_score(&answer).expect("valid score");
        assert!((out.score - 1.4).abs() < 1e-9);
        assert!((out.confidence - 0.8).abs() < 1e-9);
        assert!((out.probabilities["1"] - 0.5).abs() < 1e-9);
    }

    #[test]
    fn read_score_rejects_bad_distribution() {
        let answer = json!({
            "score": 1.0,
            "confidence": 0.8,
            "probabilities": {"0": 0.1, "1": 0.1},
        });
        assert!(read_score(&answer).is_err());
    }

    #[test]
    fn read_score_rejects_missing_fields() {
        assert!(read_score(&json!({"score": 1.0})).is_err());
    }

    /// Live sanity check against the real API. Run explicitly:
    ///   TYPESAFE_API_KEY=... cargo test qa::jev -- --ignored --nocapture
    #[test]
    #[ignore = "hits the real TypeSafe API; requires TYPESAFE_API_KEY"]
    fn jev_live_connects() {
        let key = std::env::var("TYPESAFE_API_KEY").expect("export TYPESAFE_API_KEY to run this test");
        let client = JevClient::new(key, crate::qa::DEFAULT_MODEL.to_string());
        let body = json!({
            "model": client.model,
            "state": "The sky is blue and the grass is green.",
            "questions": {
                "sky_is_blue": {
                    "type": "noul",
                    "instructions": "Does the state say the sky is blue?",
                },
            },
        });
        let started = std::time::Instant::now();
        let answers = client.answers(&body).expect("live call should succeed");
        let elapsed = started.elapsed();
        let noul_answer = answers.get("sky_is_blue").expect("answers has a sky_is_blue key");
        let noul = read_noul(noul_answer).expect("noul answer should validate");
        println!(
            "jev_live_connects: noul={noul:.4} latency={elapsed:?} calls={} input_tokens={}",
            client.calls(),
            client.input_tokens()
        );
        assert!(noul > 0.5, "expected a high probability the sky is blue, got {noul}");
    }
}
