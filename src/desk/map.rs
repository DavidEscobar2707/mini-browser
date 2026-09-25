//! OWNER: desk-core. cua-driver `get_window_state` structured output -> Observation.

use crate::qa::Observation;
use std::collections::HashMap;

/// Returns the observation and the node -> element_token map for that snapshot.
/// Rules (plan 009 §Contract): see the plan; stub returns an empty page.
pub fn observation_from_state(_state: &serde_json::Value, _app: &str) -> (Observation, HashMap<u64, String>) {
    (Observation::default(), HashMap::new())
}
