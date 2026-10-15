//! One websocket to the browser endpoint, shared by every tab through flattened sessions
//! (`Target.attachToTarget { flatten: true }`).
//!
//! Domains are never enabled implicitly: a domain is enabled only when a caller sends its
//! `enable` command. In particular `Runtime.enable` is never sent by this crate.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nodriver_cdp::{Command, Event};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::error::{Error, Result};

/// Default time limit for a command reply.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// An event as received from the browser.
#[derive(Debug, Clone)]
pub struct RawEvent {
    pub method: String,
    pub params: Value,
    pub session_id: Option<String>,
}

#[derive(Clone)]
enum MethodFilter {
    Any,
    Exact(String),
    /// Every event of a domain, e.g. `"Network."`.
    Prefix(String),
}

struct Subscriber {
    method: MethodFilter,
    /// `None` = any session (including browser-level events).
    session: Option<String>,
    tx: mpsc::UnboundedSender<Arc<RawEvent>>,
}

impl Subscriber {
    fn wants(&self, ev: &RawEvent) -> bool {
        let method_ok = match &self.method {
            MethodFilter::Any => true,
            MethodFilter::Exact(m) => &ev.method == m,
            MethodFilter::Prefix(p) => ev.method.starts_with(p.as_str()),
        };
        let session_ok = match &self.session {
            None => true,
            Some(s) => ev.session_id.as_deref() == Some(s.as_str()),
        };
        method_ok && session_ok
    }
}

type Pending = HashMap<u64, (String, oneshot::Sender<Result<Value>>)>;

struct Inner {
    next_id: AtomicU64,
    pending: Mutex<Pending>,
    subscribers: Mutex<Vec<Subscriber>>,
    out: mpsc::UnboundedSender<Message>,
    closed: AtomicBool,
    timeout: Mutex<Duration>,
}

/// Websocket connection to the browser's DevTools endpoint.
#[derive(Clone)]
pub struct Connection {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("closed", &self.is_closed())
            .finish()
    }
}

impl Connection {
    /// Connects to a DevTools websocket URL (`ws://127.0.0.1:<port>/devtools/browser/<id>`).
    pub async fn connect(ws_url: &str) -> Result<Self> {
        let config = WebSocketConfig::default()
            .max_message_size(None)
            .max_frame_size(None);
        let (ws, _) = tokio_tungstenite::connect_async_with_config(ws_url, Some(config), false).await?;
        let (mut sink, mut stream) = ws.split();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();

        let inner = Arc::new(Inner {
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            subscribers: Mutex::new(Vec::new()),
            out: out_tx,
            closed: AtomicBool::new(false),
            timeout: Mutex::new(DEFAULT_TIMEOUT),
        });

        tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                if sink.send(msg).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });

        let reader = Arc::downgrade(&inner);
        tokio::spawn(async move {
            while let Some(msg) = stream.next().await {
                let Some(inner) = reader.upgrade() else { break };
                let text = match msg {
                    Ok(Message::Text(t)) => t,
                    Ok(Message::Binary(b)) => match String::from_utf8(b.to_vec()) {
                        Ok(t) => t.into(),
                        Err(_) => continue,
                    },
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => continue,
                };
                match serde_json::from_str::<Value>(&text) {
                    Ok(v) => inner.dispatch(v),
                    Err(e) => tracing::warn!("invalid message from browser: {e}"),
                }
            }
            if let Some(inner) = reader.upgrade() {
                inner.shutdown();
            }
        });

        Ok(Self { inner })
    }

    /// Whether the websocket is closed.
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    /// Changes the default reply time limit for subsequent commands.
    pub fn set_timeout(&self, timeout: Duration) {
        *self.inner.timeout.lock().unwrap() = timeout;
    }

    /// Sends a typed command, optionally inside a target session.
    pub async fn send<C: Command>(&self, cmd: &C, session: Option<&str>) -> Result<C::Returns> {
        let params = serde_json::to_value(cmd)?;
        let v = self.call(C::METHOD, params, session).await?;
        Ok(serde_json::from_value(v)?)
    }

    /// Sends a command by name with raw JSON params and returns the raw `result`.
    pub async fn call(&self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        let timeout = *self.inner.timeout.lock().unwrap();
        self.call_with_timeout(method, params, session, timeout).await
    }

    /// Like [`Connection::call`] with an explicit time limit.
    pub async fn call_with_timeout(
        &self,
        method: &str,
        params: Value,
        session: Option<&str>,
        timeout: Duration,
    ) -> Result<Value> {
        if self.is_closed() {
            return Err(Error::Disconnected);
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = Value::String(s.to_string());
        }
        let (tx, rx) = oneshot::channel();
        self.inner
            .pending
            .lock()
            .unwrap()
            .insert(id, (method.to_string(), tx));
        if self
            .inner
            .out
            .send(Message::Text(msg.to_string().into()))
            .is_err()
        {
            self.inner.pending.lock().unwrap().remove(&id);
            return Err(Error::Disconnected);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(Error::Disconnected),
            Err(_) => {
                self.inner.pending.lock().unwrap().remove(&id);
                Err(Error::Timeout(method.to_string()))
            }
        }
    }

    /// Subscribes to one typed event. `session = None` receives it from every session.
    ///
    /// Subscribe before sending the command that triggers the event to avoid missing it.
    pub fn subscribe<E: Event>(&self, session: Option<&str>) -> EventStream<E> {
        let rx = self.add_subscriber(MethodFilter::Exact(E::METHOD.to_string()), session);
        EventStream {
            rx,
            _marker: PhantomData,
        }
    }

    /// Subscribes to raw events. `method` may be an exact name (`"Page.loadEventFired"`),
    /// a domain prefix ending in a dot (`"Network."`) or `None` for everything.
    pub fn subscribe_raw(&self, method: Option<&str>, session: Option<&str>) -> RawEventStream {
        let filter = match method {
            None => MethodFilter::Any,
            Some(m) if m.ends_with('.') => MethodFilter::Prefix(m.to_string()),
            Some(m) => MethodFilter::Exact(m.to_string()),
        };
        RawEventStream {
            rx: self.add_subscriber(filter, session),
        }
    }

    fn add_subscriber(
        &self,
        method: MethodFilter,
        session: Option<&str>,
    ) -> mpsc::UnboundedReceiver<Arc<RawEvent>> {
        let (tx, rx) = mpsc::unbounded_channel();
        if !self.is_closed() {
            self.inner.subscribers.lock().unwrap().push(Subscriber {
                method,
                session: session.map(str::to_string),
                tx,
            });
        }
        rx
    }
}

impl Inner {
    fn dispatch(&self, v: Value) {
        if let Some(id) = v.get("id").and_then(Value::as_u64) {
            let Some((method, tx)) = self.pending.lock().unwrap().remove(&id) else {
                return;
            };
            let reply = match v.get("error") {
                Some(err) => Err(Error::Protocol {
                    method,
                    code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    data: err.get("data").map(|d| match d {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    }),
                }),
                None => Ok(v.get("result").cloned().unwrap_or_else(|| json!({}))),
            };
            let _ = tx.send(reply);
            return;
        }
        let Some(method) = v.get("method").and_then(Value::as_str) else {
            return;
        };
        let ev = Arc::new(RawEvent {
            method: method.to_string(),
            params: v.get("params").cloned().unwrap_or_else(|| json!({})),
            session_id: v.get("sessionId").and_then(Value::as_str).map(str::to_string),
        });
        self.subscribers
            .lock()
            .unwrap()
            .retain(|s| !s.wants(&ev) || s.tx.send(ev.clone()).is_ok());
    }

    fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
        for (_, (_, tx)) in self.pending.lock().unwrap().drain() {
            let _ = tx.send(Err(Error::Disconnected));
        }
        self.subscribers.lock().unwrap().clear();
    }
}

/// Stream of one typed event. Ends when the connection closes.
pub struct EventStream<E> {
    rx: mpsc::UnboundedReceiver<Arc<RawEvent>>,
    _marker: PhantomData<E>,
}

impl<E: Event> EventStream<E> {
    /// Next event; `None` once the connection is closed. Events that fail to deserialize are
    /// logged and skipped.
    pub async fn next(&mut self) -> Option<E> {
        loop {
            let raw = self.rx.recv().await?;
            match serde_json::from_value(raw.params.clone()) {
                Ok(e) => return Some(e),
                Err(err) => tracing::warn!("{}: could not decode event: {err}", raw.method),
            }
        }
    }
}

/// Stream of raw events. Ends when the connection closes.
pub struct RawEventStream {
    rx: mpsc::UnboundedReceiver<Arc<RawEvent>>,
}

impl RawEventStream {
    pub async fn next(&mut self) -> Option<Arc<RawEvent>> {
        self.rx.recv().await
    }

    /// Next event without waiting, if one is queued.
    pub fn try_next(&mut self) -> Option<Arc<RawEvent>> {
        self.rx.try_recv().ok()
    }
}
