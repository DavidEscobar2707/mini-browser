//! OWNER: desk-loop. `mini --desk` / `mini --desk-observe` entry points and the loop.

use super::DeskSurface;
use crate::move_cmd::MoveReport;
use crate::qa::Judge;
use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub struct DeskArgs {
    pub goal: String,
    pub app: String,
    pub window: Option<String>,
    pub steps: usize,
    pub values: BTreeMap<String, String>,
    pub allow_writes: bool,
    pub launch: bool,
    pub hints_dir: Option<std::path::PathBuf>,
}

pub fn parse_args(_args: &[String]) -> Result<DeskArgs, String> {
    Err("--desk not implemented".into())
}

pub fn run_loop(_surface: &mut dyn DeskSurface, _judge: &dyn Judge, _a: &DeskArgs) -> Option<MoveReport> {
    None
}

/// `mini --desk ...`; exit 0 goal reached, 1 stopped without it, 2 could not run.
pub fn run(_args: &[String], _index: usize) -> i32 {
    eprintln!("mini --desk: not implemented yet (plan 009)");
    2
}

/// `mini --desk-observe --app X [--window T]`: one redacted observation as JSON, no Jev call.
pub fn observe(_args: &[String], _index: usize) -> i32 {
    eprintln!("mini --desk-observe: not implemented yet (plan 009)");
    2
}
