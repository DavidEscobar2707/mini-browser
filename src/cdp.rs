//! Minimal blocking CDP client for the live `--debug` window (`--move`,
//! `--observe`). Speaks to the content webview's remote-debugging port over a
//! plain loopback WebSocket; no async runtime.

use serde_json::{json, Value};
use std::net::TcpStream;
use std::time::{Duration, Instant};
use tungstenite::{Message, WebSocket};

/// Per-call deadline. A CDP reply that never arrives fails the call instead of
/// hanging the agent.
const CALL_TIMEOUT: Duration = Duration::from_secs(15);

/// First `type == "page"` target's WebSocket URL from a `/json/list` body.
pub fn pick_page_target(list: &Value) -> Result<String, String> {
    list.as_array()
        .ok_or_else(|| "CDP /json/list did not return an array".to_string())?
        .iter()
        .find(|t| t.get("type").and_then(Value::as_str) == Some("page"))
        .and_then(|t| t.get("webSocketDebuggerUrl").and_then(Value::as_str))
        .map(str::to_string)
        .ok_or_else(|| "CDP has no page target".to_string())
}

/// WebSocket URL of the first `type == "page"` target whose `url` is exactly `url`. Chrome for
/// Android lists every open tab; the one a run just opened on `about:blank` is the one to drive.
pub fn pick_target_with_url(list: &Value, url: &str) -> Option<String> {
    list.as_array()?
        .iter()
        .filter(|t| t.get("type").and_then(Value::as_str) == Some("page") && t.get("url").and_then(Value::as_str) == Some(url))
        // Chrome leaves the socket URL out of a target another client is attached to.
        .find_map(|t| t.get("webSocketDebuggerUrl").and_then(Value::as_str))
        .map(str::to_string)
}

fn target_list(port: u16) -> Result<Value, String> {
    ureq::get(&format!("http://127.0.0.1:{port}/json/list"))
        .call()
        .map_err(|e| format!("CDP /json/list failed: {e}"))?
        .body_mut()
        .read_json()
        .map_err(|e| format!("CDP /json/list was not JSON: {e}"))
}

pub fn page_ws_url(port: u16) -> Result<String, String> {
    pick_page_target(&target_list(port)?)
}

/// The page whose URL is `url` (the tab a run just opened on `about:blank`), else the first
/// page target.
pub fn page_ws_url_for(port: u16, url: &str) -> Result<String, String> {
    let list = target_list(port)?;
    pick_target_with_url(&list, url).map(Ok).unwrap_or_else(|| pick_page_target(&list))
}

pub struct Cdp {
    socket: WebSocket<TcpStream>,
    next_id: u64,
    ws_url: String,
}

/// Prefix of every error caused by a reply that never came. Callers treat it
/// as a stale page (jev-ultrafast `StalePage`): reconnect and observe again.
pub const TIMEOUT: &str = "cdp-timeout";

fn open_socket(ws_url: &str) -> Result<WebSocket<TcpStream>, String> {
    let authority = ws_url
        .strip_prefix("ws://")
        .and_then(|rest| rest.split('/').next())
        .ok_or_else(|| format!("not a loopback ws:// URL: {ws_url}"))?;
    let stream = TcpStream::connect(authority).map_err(|e| format!("CDP connect failed: {e}"))?;
    stream.set_read_timeout(Some(CALL_TIMEOUT)).map_err(|e| e.to_string())?;
    let (socket, _) = tungstenite::client(ws_url, stream).map_err(|e| format!("CDP handshake failed: {e}"))?;
    Ok(socket)
}

impl Cdp {
    pub fn connect(ws_url: &str) -> Result<Cdp, String> {
        Ok(Cdp { socket: open_socket(ws_url)?, next_id: 1, ws_url: ws_url.to_string() })
    }

    /// Drops the socket and opens a fresh one to the same target. A reply
    /// lost during a navigation leaves the old socket mid-frame.
    pub fn reconnect(&mut self) -> Result<(), String> {
        self.socket = open_socket(&self.ws_url)?;
        Ok(())
    }

    /// Sends one method and returns its `result`. Events are skipped.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.call_within(method, params, CALL_TIMEOUT)
    }

    pub fn call_within(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let _ = self.socket.get_mut().set_read_timeout(Some(timeout));
        let msg = json!({ "id": id, "method": method, "params": params });
        self.socket.send(Message::text(msg.to_string())).map_err(|e| format!("{method}: send failed: {e}"))?;
        let deadline = Instant::now() + timeout;
        loop {
            if Instant::now() > deadline {
                return Err(format!("{TIMEOUT}: {method} got no reply within {}ms", timeout.as_millis()));
            }
            let frame = match self.socket.read() {
                Ok(f) => f,
                Err(tungstenite::Error::Io(e))
                    if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
                {
                    return Err(format!("{TIMEOUT}: {method} got no reply within {}ms", timeout.as_millis()));
                }
                Err(e) => return Err(format!("{method}: read failed: {e}")),
            };
            let Message::Text(text) = frame else { continue };
            let Ok(reply) = serde_json::from_str::<Value>(text.as_str()) else { continue };
            if reply.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(err) = reply.get("error") {
                let message = err.get("message").and_then(Value::as_str).unwrap_or("unknown error");
                return Err(format!("{method}: {message}"));
            }
            return Ok(reply.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// `Runtime.evaluate` with a custom deadline.
    pub fn eval_within(&mut self, expression: &str, timeout: Duration) -> Result<String, String> {
        let result = self.call_within(
            "Runtime.evaluate",
            json!({ "expression": expression, "returnByValue": true, "awaitPromise": true }),
            timeout,
        )?;
        eval_value(&result)
    }

    /// `Runtime.evaluate` with `awaitPromise` + `returnByValue`; returns the
    /// value serialized as JSON text (the shape `qa::probes` parses).
    pub fn eval(&mut self, expression: &str) -> Result<String, String> {
        let result = self.call(
            "Runtime.evaluate",
            json!({ "expression": expression, "returnByValue": true, "awaitPromise": true }),
        )?;
        eval_value(&result)
    }

    pub fn mouse_click(&mut self, x: f64, y: f64) -> Result<(), String> {
        let base = json!({ "x": x, "y": y, "button": "left", "buttons": 1, "clickCount": 1 });
        let mut moved = base.clone();
        moved["type"] = json!("mouseMoved");
        moved["buttons"] = json!(0);
        self.call("Input.dispatchMouseEvent", moved)?;
        let mut press = base.clone();
        press["type"] = json!("mousePressed");
        self.call("Input.dispatchMouseEvent", press)?;
        let mut release = base;
        release["type"] = json!("mouseReleased");
        self.call("Input.dispatchMouseEvent", release)?;
        Ok(())
    }

    /// Ctrl+A through the editing command (replaces the field's contents), as
    /// jev-ultrafast does before `Input.insertText`.
    pub fn select_all(&mut self) -> Result<(), String> {
        self.call(
            "Input.dispatchKeyEvent",
            json!({ "type": "keyDown", "key": "a", "code": "KeyA", "modifiers": 2, "commands": ["selectAll"] }),
        )?;
        self.call("Input.dispatchKeyEvent", json!({ "type": "keyUp", "key": "a", "code": "KeyA", "modifiers": 2 }))?;
        Ok(())
    }

    pub fn wheel(&mut self, x: f64, y: f64, delta_y: f64) -> Result<(), String> {
        self.call(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": 0, "deltaY": delta_y }),
        )?;
        Ok(())
    }

    /// Enter on the focused element (submits a search box with no button).
    pub fn press_enter(&mut self) -> Result<(), String> {
        let down = json!({ "type": "keyDown", "key": "Enter", "code": "Enter", "windowsVirtualKeyCode": 13, "text": "\r" });
        self.call("Input.dispatchKeyEvent", down)?;
        self.call("Input.dispatchKeyEvent", json!({ "type": "keyUp", "key": "Enter", "code": "Enter", "windowsVirtualKeyCode": 13 }))?;
        Ok(())
    }

    pub fn key_text(&mut self, text: &str) -> Result<(), String> {
        if self.call("Input.insertText", json!({ "text": text })).is_ok() {
            return Ok(());
        }
        for ch in text.chars() {
            let s = ch.to_string();
            self.call("Input.dispatchKeyEvent", json!({ "type": "keyDown", "text": s, "unmodifiedText": s }))?;
            self.call("Input.dispatchKeyEvent", json!({ "type": "keyUp", "text": s, "unmodifiedText": s }))?;
        }
        Ok(())
    }

    pub fn navigate(&mut self, url: &str) -> Result<(), String> {
        let result = self.call("Page.navigate", json!({ "url": url }))?;
        match result.get("errorText").and_then(Value::as_str) {
            Some(err) if !err.is_empty() => Err(format!("navigation failed: {err}")),
            _ => Ok(()),
        }
    }

    /// Polls `document.readyState` until `complete` or the timeout.
    pub fn wait_for_load(&mut self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(state) = self.eval("document.readyState") {
                if state.trim_matches('"') == "complete" {
                    return Ok(());
                }
            }
            if Instant::now() > deadline {
                return Err("page did not finish loading".to_string());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn eval_value(result: &Value) -> Result<String, String> {
    if let Some(details) = result.get("exceptionDetails") {
        let text = details
            .pointer("/exception/description")
            .or_else(|| details.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("script threw");
        return Err(format!("eval failed: {text}"));
    }
    let inner = result.get("result").ok_or("eval returned no result")?;
    if let Some(value) = inner.get("value") {
        return Ok(value.to_string());
    }
    if inner.get("type").and_then(Value::as_str) == Some("undefined") {
        return Ok("null".to_string());
    }
    Ok(inner.get("description").and_then(Value::as_str).unwrap_or("null").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_first_page_target() {
        let list = json!([
            {"type": "service_worker", "webSocketDebuggerUrl": "ws://127.0.0.1:9222/devtools/sw"},
            {"type": "page", "webSocketDebuggerUrl": "ws://127.0.0.1:9222/devtools/page/A"},
            {"type": "page", "webSocketDebuggerUrl": "ws://127.0.0.1:9222/devtools/page/B"},
        ]);
        assert_eq!(pick_page_target(&list).unwrap(), "ws://127.0.0.1:9222/devtools/page/A");
        assert!(pick_page_target(&json!([])).is_err());
        assert!(pick_page_target(&json!([{"type": "iframe"}])).is_err());
    }

    #[test]
    fn prefers_the_page_whose_url_matches() {
        let list = json!([
            {"type": "page", "url": "https://example.com/", "webSocketDebuggerUrl": "ws://127.0.0.1:9333/devtools/page/A"},
            {"type": "service_worker", "url": "about:blank", "webSocketDebuggerUrl": "ws://127.0.0.1:9333/devtools/sw"},
            // Chrome leaves the socket URL out of a target another client is attached to.
            {"type": "page", "url": "about:blank"},
            {"type": "page", "url": "about:blank", "webSocketDebuggerUrl": "ws://127.0.0.1:9333/devtools/page/B"},
            {"type": "page", "url": "about:blank", "webSocketDebuggerUrl": "ws://127.0.0.1:9333/devtools/page/C"},
        ]);
        assert_eq!(pick_target_with_url(&list, "about:blank").as_deref(), Some("ws://127.0.0.1:9333/devtools/page/B"));
        assert_eq!(pick_target_with_url(&list, "https://example.com/").as_deref(), Some("ws://127.0.0.1:9333/devtools/page/A"));
        // No tab has this URL: None, so the caller falls back to `pick_page_target`.
        assert_eq!(pick_target_with_url(&list, "https://other.example/"), None);
        assert_eq!(pick_target_with_url(&json!([]), "about:blank"), None);
        assert_eq!(pick_target_with_url(&json!({"type": "page"}), "about:blank"), None, "not a list");
    }

    #[test]
    fn eval_value_returns_json_text_or_the_exception() {
        assert_eq!(eval_value(&json!({"result": {"type": "number", "value": 2}})).unwrap(), "2");
        assert_eq!(eval_value(&json!({"result": {"type": "object", "value": {"ok": true}}})).unwrap(), r#"{"ok":true}"#);
        assert_eq!(eval_value(&json!({"result": {"type": "undefined"}})).unwrap(), "null");
        let thrown = json!({"result": {}, "exceptionDetails": {"exception": {"description": "ReferenceError: x"}}});
        assert!(eval_value(&thrown).unwrap_err().contains("ReferenceError"));
    }
}
