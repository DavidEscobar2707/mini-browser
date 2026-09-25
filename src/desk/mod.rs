//! `mini --desk`: the `--move` loop (Jev picks the control, one evidence gate,
//! episodes) on native Windows apps, through cua-driver's UIA tree.
//! Contract: plans/009-desktop-move.md. Owners: client/map/redact = worker `desk-core`;
//! run = worker `desk-loop`. This file is the shared contract (orchestrator owns it).

pub mod client;
pub mod map;
pub mod redact;
pub mod run;

use crate::qa::Observation;

/// The window a run drives. `app` is cua-driver's app name (e.g. "Calculator").
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowRef {
    pub pid: u64,
    pub window_id: u64,
    pub app: String,
    pub title: String,
}

/// One input. `node` is `ObservedElement.node` from the LAST `observe()`
/// (= cua-driver `element_index`); the surface maps it to that snapshot's
/// `element_token` itself. Tokens never leave the surface.
#[derive(Clone, Debug, PartialEq)]
pub enum DeskAction {
    Click { node: u64 },
    SetValue { node: u64, value: String },
    TypeText { node: u64, text: String },
    /// cua-driver key names: "return", "escape", "tab", ...
    PressKey { key: String },
    Scroll { node: Option<u64>, down: bool },
}

/// cua-driver's verdict on one input.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActResult {
    /// "confirmed" | "unverifiable" | "suspected_noop" | "refused"
    pub effect: String,
    /// Refusal code or driver summary, already redacted.
    pub note: Option<String>,
}

/// What the loop drives. `client::CuaDesk` is the live one; tests use a fake.
pub trait DeskSurface {
    /// Fresh snapshot of the attached window: redacted, secret fields dropped.
    /// Replaces the node -> token map (cua-driver: indices die with each snapshot).
    fn observe(&mut self) -> Result<Observation, String>;
    fn act(&mut self, action: &DeskAction) -> Result<ActResult, String>;
    fn window(&self) -> &WindowRef;
}
