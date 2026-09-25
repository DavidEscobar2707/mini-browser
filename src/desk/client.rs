//! OWNER: desk-core. Long-lived `cua-driver mcp` stdio session (JSON-RPC 2.0).
//! The one-shot `cua-driver call` CLI makes every element_token stale: do not use it.

use super::{ActResult, DeskAction, DeskSurface, WindowRef};
use crate::qa::Observation;

pub struct CuaDesk {
    window: WindowRef,
}

impl CuaDesk {
    /// Spawn `cua-driver mcp`, send `initialize` + `notifications/initialized`.
    pub fn connect() -> Result<CuaDesk, String> {
        Err("desk client not implemented".into())
    }
    /// `launch_app {name}` (background, no focus steal), then wait up to 15 s for its window.
    pub fn launch(&mut self, _app: &str) -> Result<(), String> {
        Err("not implemented".into())
    }
    /// `list_windows`; first on-screen window whose title contains `app` (or
    /// `title` when given), case-insensitive. Refused windows (redact::refused_window) -> Err.
    pub fn find_window(&mut self, _app: &str, _title: Option<&str>) -> Result<WindowRef, String> {
        Err("not implemented".into())
    }
    pub fn attach(&mut self, window: WindowRef) {
        self.window = window;
    }
}

impl DeskSurface for CuaDesk {
    fn observe(&mut self) -> Result<Observation, String> {
        Err("not implemented".into())
    }
    fn act(&mut self, _action: &DeskAction) -> Result<ActResult, String> {
        Err("not implemented".into())
    }
    fn window(&self) -> &WindowRef {
        &self.window
    }
}
