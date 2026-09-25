//! OWNER: desk-core. Long-lived `cua-driver mcp` stdio session (JSON-RPC 2.0).
//! The one-shot `cua-driver call` CLI makes every element_token stale: do not use it.

use super::redact::{redact, refused_window};
use super::{map, ActResult, DeskAction, DeskSurface, WindowRef};
use crate::qa::Observation;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

const REPLY_TIMEOUT: Duration = Duration::from_secs(30);
const LAUNCH_WAIT: Duration = Duration::from_secs(15);
/// GAP: plan names `amount` without a value; three notches per scroll.
const SCROLL_AMOUNT: u64 = 3;

pub struct CuaDesk {
    window: WindowRef,
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
    /// node -> element_token of the last snapshot. Tokens never leave this struct.
    tokens: HashMap<u64, String>,
}

/// One JSON-RPC line (no trailing newline). `id: None` = notification.
fn request_line(id: Option<u64>, method: &str, params: Option<Value>) -> String {
    let mut m = json!({"jsonrpc": "2.0", "method": method});
    if let Some(id) = id {
        m["id"] = json!(id);
    }
    if let Some(p) = params {
        m["params"] = p;
    }
    m.to_string()
}

/// The reply for `id` in `line`, or None for notifications, other ids, and non-JSON noise.
fn match_reply(line: &str, id: u64) -> Option<Result<Value, String>> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("id")?.as_u64()? != id {
        return None;
    }
    Some(match v.get("error") {
        Some(e) => Err(redact(e["message"].as_str().unwrap_or("json-rpc error"))),
        None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
    })
}

/// `tools/call` result -> the tool's payload: `structuredContent`, else `result` itself.
/// `isError` results become Err with their (redacted) text.
fn tool_payload(result: Value) -> Result<Value, String> {
    if result["isError"].as_bool() == Some(true) {
        let text = result["content"][0]["text"].as_str().unwrap_or("tool error");
        return Err(redact(text));
    }
    Ok(match result.get("structuredContent") {
        Some(sc) if !sc.is_null() => sc.clone(),
        _ => result,
    })
}

/// click/set_value/... payload -> ActResult. Refusals keep only their code.
fn act_result(payload: &Value) -> ActResult {
    if payload["status"].as_str() == Some("refused") {
        let code = payload["refusal"]["code"].as_str().unwrap_or("refused");
        return ActResult { effect: "refused".into(), note: Some(redact(code)) };
    }
    ActResult {
        effect: payload["effect"].as_str().unwrap_or("unverifiable").into(),
        note: payload["summary"].as_str().map(redact),
    }
}

/// DeskAction -> (tool name, arguments), with the node's token from the last snapshot.
fn action_call(w: &WindowRef, tokens: &HashMap<u64, String>, action: &DeskAction) -> Result<(&'static str, Value), String> {
    let token = |node: u64| tokens.get(&node).cloned().ok_or_else(|| format!("node {node} not in the last snapshot"));
    let mut args = json!({"pid": w.pid, "window_id": w.window_id});
    let name = match action {
        DeskAction::Click { node } => {
            args["element_token"] = json!(token(*node)?);
            "click"
        }
        DeskAction::SetValue { node, value } => {
            args["element_token"] = json!(token(*node)?);
            args["value"] = json!(value);
            "set_value"
        }
        DeskAction::TypeText { node, text } => {
            args["element_token"] = json!(token(*node)?);
            args["text"] = json!(text);
            "type_text"
        }
        DeskAction::PressKey { key } => {
            args["key"] = json!(key);
            "press_key"
        }
        DeskAction::Scroll { node, down } => {
            if let Some(n) = node {
                args["element_token"] = json!(token(*n)?);
            }
            args["direction"] = json!(if *down { "down" } else { "up" });
            args["amount"] = json!(SCROLL_AMOUNT);
            "scroll"
        }
    };
    Ok((name, args))
}

/// First on-screen, non-minimised window whose title contains `needle` (case-insensitive).
fn pick_window(list: &Value, app: &str, needle: &str) -> Option<WindowRef> {
    let needle = needle.to_lowercase();
    list["_legacy_windows"].as_array()?.iter().find_map(|w| {
        let title = w["title"].as_str().unwrap_or("");
        let visible = w["is_on_screen"].as_bool() != Some(false) && w["minimized"].as_bool() != Some(true);
        (visible && title.to_lowercase().contains(&needle)).then(|| WindowRef {
            pid: w["pid"].as_u64().unwrap_or(0),
            window_id: w["window_id"].as_u64().unwrap_or(0),
            app: app.to_string(),
            title: title.to_string(),
        })
    })
}

impl CuaDesk {
    /// Spawn `cua-driver mcp`, send `initialize` + `notifications/initialized`.
    pub fn connect() -> Result<CuaDesk, String> {
        let mut cmd = Command::new("cua-driver");
        cmd.arg("mcp").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000); // CREATE_NO_WINDOW
        let mut child = cmd.spawn().map_err(|e| format!("cua-driver mcp: {e}"))?;
        let stdin = child.stdin.take().ok_or("cua-driver: no stdin")?;
        let stdout = child.stdout.take().ok_or("cua-driver: no stdout")?;
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut desk = CuaDesk { window: WindowRef::default(), child, stdin, lines, next_id: 1, tokens: HashMap::new() };
        desk.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "mini", "version": env!("CARGO_PKG_VERSION")}}),
        )?;
        desk.send(&request_line(None, "notifications/initialized", None))?;
        Ok(desk)
    }

    fn send(&mut self, line: &str) -> Result<(), String> {
        writeln!(self.stdin, "{line}").and_then(|_| self.stdin.flush()).map_err(|e| format!("cua-driver write: {e}"))
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&request_line(Some(id), method, Some(params)))?;
        let deadline = Instant::now() + REPLY_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    if let Some(reply) = match_reply(&line, id) {
                        return reply;
                    }
                }
                Err(RecvTimeoutError::Timeout) => return Err(format!("cua-driver: no reply to {method} in 30 s")),
                Err(RecvTimeoutError::Disconnected) => return Err("cua-driver exited".into()),
            }
        }
    }

    fn call_tool(&mut self, name: &str, args: Value) -> Result<Value, String> {
        tool_payload(self.request("tools/call", json!({"name": name, "arguments": args}))?)
    }

    /// `launch_app {name}` (background, no focus steal), then wait up to 15 s for its window.
    pub fn launch(&mut self, app: &str) -> Result<(), String> {
        if let Some(why) = refused_window(app, "") {
            return Err(format!("refused window: {why}"));
        }
        self.call_tool("launch_app", json!({"name": app}))?;
        let deadline = Instant::now() + LAUNCH_WAIT;
        loop {
            match self.find_window(app, None) {
                Ok(w) => {
                    self.attach(w);
                    return Ok(());
                }
                Err(e) if Instant::now() >= deadline => return Err(e),
                Err(_) => std::thread::sleep(Duration::from_millis(500)),
            }
        }
    }

    /// `list_windows`; first on-screen window whose title contains `app` (or
    /// `title` when given), case-insensitive. Refused windows (redact::refused_window) -> Err.
    pub fn find_window(&mut self, app: &str, title: Option<&str>) -> Result<WindowRef, String> {
        if let Some(why) = refused_window(app, title.unwrap_or("")) {
            return Err(format!("refused window: {why}"));
        }
        let list = self.call_tool("list_windows", json!({}))?;
        let w = pick_window(&list, app, title.unwrap_or(app)).ok_or_else(|| format!("no on-screen window for {app}"))?;
        if let Some(why) = refused_window(app, &w.title) {
            return Err(format!("refused window: {why}"));
        }
        Ok(WindowRef { title: redact(&w.title), ..w })
    }

    pub fn attach(&mut self, window: WindowRef) {
        self.window = window;
        self.tokens.clear();
    }
}

impl Drop for CuaDesk {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl DeskSurface for CuaDesk {
    fn observe(&mut self) -> Result<Observation, String> {
        let args = json!({"pid": self.window.pid, "window_id": self.window.window_id, "include_screenshot": false});
        let state = self.call_tool("get_window_state", args)?;
        let (app, title) = (state["app_name"].as_str().unwrap_or(""), state["window_title"].as_str().unwrap_or(""));
        if let Some(why) = refused_window(app, title) {
            self.tokens.clear();
            return Err(format!("refused window: {why}"));
        }
        let (obs, tokens) = map::observation_from_state(&state, &self.window.app);
        self.tokens = tokens;
        Ok(obs)
    }

    fn act(&mut self, action: &DeskAction) -> Result<ActResult, String> {
        let (name, args) = action_call(&self.window, &self.tokens, action)?;
        Ok(act_result(&self.call_tool(name, args)?))
    }

    fn window(&self) -> &WindowRef {
        &self.window
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_lines() {
        let v: Value = serde_json::from_str(&request_line(Some(7), "tools/call", Some(json!({"name": "click"})))).unwrap();
        assert_eq!(v, json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "click"}}));
        let n = request_line(None, "notifications/initialized", None);
        assert_eq!(n, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        assert!(!n.contains('\n'));
    }

    #[test]
    fn reply_matcher_skips_noise() {
        assert!(match_reply(r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{}}"#, 3).is_none());
        assert!(match_reply(r#"{"jsonrpc":"2.0","id":2,"result":{}}"#, 3).is_none());
        assert!(match_reply("cua-driver starting...", 3).is_none());
        assert_eq!(match_reply(r#"{"jsonrpc":"2.0","id":3,"result":{"ok":1}}"#, 3), Some(Ok(json!({"ok": 1}))));
        let err = match_reply(r#"{"id":3,"error":{"code":-1,"message":"bad token=FAKEsecret"}}"#, 3).unwrap().unwrap_err();
        assert!(!err.contains("FAKEsecret"));
    }

    #[test]
    fn tool_result_unwrap() {
        let r = json!({"content": [{"type": "text", "text": "x"}], "structuredContent": {"effect": "confirmed"}});
        assert_eq!(tool_payload(r).unwrap(), json!({"effect": "confirmed"}));
        let bare = json!({"_legacy_windows": []});
        assert_eq!(tool_payload(bare.clone()).unwrap(), bare);
        let e = json!({"isError": true, "content": [{"type": "text", "text": "no such window"}]});
        assert_eq!(tool_payload(e).unwrap_err(), "no such window");
    }

    #[test]
    fn refusal_and_effect_to_act_result() {
        let r = act_result(&json!({"status": "refused", "refusal": {"code": "stale_element_token", "message": "m"}}));
        assert_eq!(r, ActResult { effect: "refused".into(), note: Some("stale_element_token".into()) });
        let ok = act_result(&json!({"delivery": {"mode": "background"}, "effect": "unverifiable",
            "route": "accessibility", "summary": "Invoked Seven; api key FAKEvalue"}));
        assert_eq!(ok.effect, "unverifiable");
        assert!(!ok.note.unwrap().contains("FAKEvalue"));
    }

    #[test]
    fn actions_use_snapshot_tokens() {
        let w = WindowRef { pid: 9580, window_id: 460848, app: "Calculator".into(), title: "Calculator".into() };
        let tokens = HashMap::from([(31u64, "s00000002:31".to_string())]);
        let (name, args) = action_call(&w, &tokens, &DeskAction::Click { node: 31 }).unwrap();
        assert_eq!((name, args), ("click", json!({"pid": 9580, "window_id": 460848, "element_token": "s00000002:31"})));
        let (name, args) = action_call(&w, &tokens, &DeskAction::SetValue { node: 31, value: "5".into() }).unwrap();
        assert_eq!((name, &args["value"]), ("set_value", &json!("5")));
        let (name, args) = action_call(&w, &tokens, &DeskAction::PressKey { key: "return".into() }).unwrap();
        assert_eq!((name, args.get("element_token")), ("press_key", None));
        let (_, args) = action_call(&w, &tokens, &DeskAction::Scroll { node: None, down: true }).unwrap();
        assert_eq!(args["direction"], "down");
        let err = action_call(&w, &tokens, &DeskAction::Click { node: 4 }).unwrap_err();
        assert_eq!(err, "node 4 not in the last snapshot");
    }

    #[test]
    fn picks_visible_window_by_title() {
        let list = json!({"_legacy_windows": [
            {"pid": 1, "window_id": 10, "title": "Calculator", "is_on_screen": false, "minimized": false},
            {"pid": 2, "window_id": 20, "title": "Calculator", "is_on_screen": true, "minimized": false},
        ]});
        assert_eq!(pick_window(&list, "Calculator", "calc").unwrap().window_id, 20);
        assert!(pick_window(&list, "Settings", "Settings").is_none());
    }
}
