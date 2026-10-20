//! CDP over a websocket with events: one reader thread owns the socket, `call`
//! waits on a per-id channel, every event goes to the registered handlers.
//! The loopback debug window keeps using the simpler `cdp.rs` client.
// `dead_code` until `qa::remote` (plan 010 B3) builds on this.
#![allow(dead_code)]

use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::TcpStream;
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::Duration;
use tungstenite::{Message, WebSocket};

type Handler = Arc<dyn Fn(&str, &Value, &CdpBus) + Send + Sync>;

const CALL_TIMEOUT: Duration = Duration::from_secs(20);
/// How long the reader holds the socket lock while it waits for data. `call` needs that lock to
/// send, so each call waits out the rest of one poll: a 50 ms poll measured 62 ms per call. Windows
/// rounds socket timeouts up to its 15.6 ms timer tick, so about 16 ms per call is the floor.
const POLL: Duration = Duration::from_millis(5);
/// The upgrade reply can take longer than one `POLL` (Chrome behind `adb forward`).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct CdpBus {
    socket: Arc<Mutex<WebSocket<TcpStream>>>,
    pending: Arc<Mutex<HashMap<u64, Sender<Value>>>>,
    next_id: Arc<Mutex<u64>>,
    handlers: Arc<Mutex<Vec<Handler>>>,
}

impl CdpBus {
    pub fn connect(ws_url: &str) -> Result<CdpBus, String> {
        let authority = ws_url
            .strip_prefix("ws://")
            .and_then(|rest| rest.split('/').next())
            .ok_or_else(|| format!("not a ws:// URL: {ws_url}"))?;
        let stream = TcpStream::connect(authority).map_err(|e| format!("CDP connect failed: {e}"))?;
        // A read timeout inside the handshake is a failure, so wait long for it and poll after it.
        stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).map_err(|e| e.to_string())?;
        let (socket, _) = tungstenite::client(ws_url, stream).map_err(|e| format!("CDP handshake failed: {e}"))?;
        socket.get_ref().set_read_timeout(Some(POLL)).map_err(|e| e.to_string())?;
        let bus = CdpBus {
            socket: Arc::new(Mutex::new(socket)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(Mutex::new(1)),
            handlers: Arc::new(Mutex::new(Vec::new())),
        };
        let reader = bus.clone();
        std::thread::Builder::new()
            .name("cdp-bus-reader".into())
            .spawn(move || reader.read_loop())
            .map_err(|e| format!("cdp-bus thread: {e}"))?;
        Ok(bus)
    }

    /// Register an event handler. Handlers run on the reader thread: they must not
    /// call `call` (deadlock); use `fire`.
    pub fn on_event(&self, f: impl Fn(&str, &Value, &CdpBus) + Send + Sync + 'static) {
        self.handlers.lock().push(Arc::new(f));
    }

    fn send_frame(&self, id: u64, method: &str, params: &Value) -> Result<(), String> {
        let frame = json!({ "id": id, "method": method, "params": params }).to_string();
        self.socket.lock().send(Message::Text(frame.into())).map_err(|e| format!("CDP send failed: {e}"))
    }

    /// Send and forget: the reply is dropped. Safe from inside an event handler.
    pub fn fire(&self, method: &str, params: Value) -> Result<(), String> {
        let id = self.take_id();
        self.send_frame(id, method, &params)
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.take_id();
        let (tx, rx) = channel();
        self.pending.lock().insert(id, tx);
        self.send_frame(id, method, &params)?;
        let reply = rx.recv_timeout(CALL_TIMEOUT).map_err(|_| {
            self.pending.lock().remove(&id);
            format!("cdp-timeout: {method} got no reply in {} s", CALL_TIMEOUT.as_secs())
        })?;
        if let Some(err) = reply.get("error") {
            return Err(format!("{method}: {}", err.get("message").and_then(Value::as_str).unwrap_or("CDP error")));
        }
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    }

    fn take_id(&self) -> u64 {
        let mut n = self.next_id.lock();
        let id = *n;
        *n += 1;
        id
    }

    fn read_loop(self) {
        loop {
            let msg = {
                let mut sock = self.socket.lock();
                sock.read()
            };
            match msg {
                Ok(Message::Text(text)) => self.dispatch(&text),
                Ok(Message::Close(_)) | Err(tungstenite::Error::ConnectionClosed) | Err(tungstenite::Error::AlreadyClosed) => return,
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(_) => return,
            }
        }
    }

    fn dispatch(&self, text: &str) {
        let Ok(v) = serde_json::from_str::<Value>(text) else { return };
        if let Some(id) = v.get("id").and_then(Value::as_u64) {
            if let Some(tx) = self.pending.lock().remove(&id) {
                let _ = tx.send(v);
            }
            return;
        }
        if let Some(method) = v.get("method").and_then(Value::as_str) {
            let params = v.get("params").cloned().unwrap_or(Value::Null);
            let handlers: Vec<Handler> = self.handlers.lock().clone();
            for h in handlers {
                h(method, &params, self);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A one-connection fake CDP server: on the first call it emits one event,
    /// then replies; on the second call it replies with an error.
    fn fake_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            let mut n = 0;
            while let Ok(Message::Text(text)) = ws.read() {
                let req: Value = serde_json::from_str(&text).unwrap();
                let id = req["id"].as_u64().unwrap();
                n += 1;
                if n == 1 {
                    ws.send(Message::Text(json!({"method":"Test.event","params":{"n":1}}).to_string().into())).unwrap();
                    ws.send(Message::Text(json!({"id":id,"result":{"value":42}}).to_string().into())).unwrap();
                } else {
                    ws.send(Message::Text(json!({"id":id,"error":{"message":"nope"}}).to_string().into())).unwrap();
                }
            }
        });
        format!("ws://{addr}/devtools/page/1")
    }

    #[test]
    fn calls_get_their_reply_and_events_reach_handlers() {
        let bus = CdpBus::connect(&fake_server()).unwrap();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = seen.clone();
        bus.on_event(move |m, p, _| sink.lock().push(format!("{m}:{}", p["n"])));
        let r = bus.call("Runtime.evaluate", json!({"expression": "1"})).unwrap();
        assert_eq!(r["value"], 42);
        let err = bus.call("Page.navigate", json!({})).unwrap_err();
        assert!(err.contains("nope"), "{err}");
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(*seen.lock(), vec!["Test.event:1".to_string()]);
    }

    /// A fake CDP server that waits `handshake_delay` before it answers the websocket
    /// upgrade, then answers every call at once with `{"ok": true}`.
    fn echo_server(handshake_delay: Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            std::thread::sleep(handshake_delay);
            let mut ws = tungstenite::accept(stream).unwrap();
            while let Ok(Message::Text(text)) = ws.read() {
                let req: Value = serde_json::from_str(&text).unwrap();
                ws.send(Message::Text(json!({"id": req["id"], "result": {"ok": true}}).to_string().into())).unwrap();
            }
        });
        format!("ws://{addr}/devtools/page/1")
    }

    #[test]
    fn connect_waits_for_a_slow_handshake() {
        // Chrome behind `adb forward` can need longer than the reader's `POLL` to answer the upgrade.
        let bus = CdpBus::connect(&echo_server(Duration::from_millis(300))).unwrap();
        assert_eq!(bus.call("Runtime.evaluate", json!({})).unwrap()["ok"], true);
    }

    #[test]
    fn sequential_calls_do_not_wait_for_the_reader_poll() {
        // `call` needs the socket lock that the reader holds while it waits for data. With a 50 ms
        // read timeout every call paid it in full (62 ms on Windows, which rounds the timeout up
        // to its 15.6 ms timer tick): 20 calls took 1.25 s.
        let bus = CdpBus::connect(&echo_server(Duration::ZERO)).unwrap();
        let start = std::time::Instant::now();
        for _ in 0..20 {
            bus.call("Runtime.evaluate", json!({})).unwrap();
        }
        let took = start.elapsed();
        assert!(took < Duration::from_millis(750), "20 sequential calls took {took:?}");
    }
}
