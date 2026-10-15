//! One websocket to the browser endpoint, shared by every tab through flattened sessions
//! (`Target.attachToTarget { flatten: true }`).
//!
//! Domains are never enabled implicitly: a domain is enabled only when a caller sends its
//! `enable` command. In particular `Runtime.enable` is never sent by this crate.
//!
//! # Reconnection
//!
//! When the websocket drops while the browser is still running, it is reopened in the background
//! right away (and by the next command, if that runs first), with up to [`RECONNECT_ATTEMPTS`]
//! tries, and a new session is attached to every target registered with
//! [`Connection::attach`]. When every try fails (the browser is gone), event streams end and
//! commands fail with [`Error::Disconnected`]. After a reconnection:
//!
//! - commands that were waiting for a reply when the link dropped fail with
//!   [`Error::Disconnected`]; they are not resent, since the browser may have run them;
//! - subscriptions made per target ([`Connection::subscribe_target`], used by `Tab`) and
//!   browser-wide subscriptions keep receiving events; subscriptions bound to a raw session id
//!   stop receiving, because the session no longer exists;
//! - domains enabled with `X.enable` are disabled in the new sessions and must be enabled again;
//! - a [`RECONNECTED_EVENT`] is delivered to subscribers that match it.
//!
//! [`Connection::set_auto_reconnect`] turns this off; `Browser::close` does so before closing.

use std::collections::HashMap;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
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
/// Websocket open attempts per reconnection.
pub const RECONNECT_ATTEMPTS: u32 = 3;
/// Synthetic event delivered after a reconnection; `params` is `{"generation": n}`.
pub const RECONNECTED_EVENT: &str = "Nodriver.reconnected";

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

#[derive(Clone)]
enum Scope {
    /// Every event, browser-level ones included.
    All,
    /// Events of one session id.
    Session(String),
    /// Events of whatever session is currently attached to a target.
    Target(String),
}

struct Subscriber {
    method: MethodFilter,
    scope: Scope,
    tx: mpsc::UnboundedSender<Arc<RawEvent>>,
}

impl Subscriber {
    fn wants(&self, ev: &RawEvent, target: Option<&str>) -> bool {
        let method_ok = match &self.method {
            MethodFilter::Any => true,
            MethodFilter::Exact(m) => &ev.method == m,
            MethodFilter::Prefix(p) => ev.method.starts_with(p.as_str()),
        };
        let scope_ok = match &self.scope {
            Scope::All => true,
            Scope::Session(s) => ev.session_id.as_deref() == Some(s.as_str()),
            Scope::Target(t) => target == Some(t.as_str()),
        };
        method_ok && scope_ok
    }
}

struct PendingCall {
    method: String,
    generation: u64,
    tx: oneshot::Sender<Result<Value>>,
}

/// The current websocket. `out` is `None` while disconnected.
struct Link {
    out: Option<mpsc::UnboundedSender<Message>>,
    generation: u64,
}

#[derive(Default)]
struct Sessions {
    by_target: HashMap<String, String>,
    by_session: HashMap<String, String>,
}

struct Inner {
    ws_url: String,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, PendingCall>>,
    subscribers: Mutex<Vec<Subscriber>>,
    link: Mutex<Link>,
    sessions: Mutex<Sessions>,
    reconnect_lock: tokio::sync::Mutex<()>,
    auto_reconnect: AtomicBool,
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
            .field("ws_url", &self.inner.ws_url)
            .field("closed", &self.is_closed())
            .field("generation", &self.generation())
            .finish()
    }
}

impl Connection {
    /// Connects to a DevTools websocket URL (`ws://127.0.0.1:<port>/devtools/browser/<id>`).
    pub async fn connect(ws_url: &str) -> Result<Self> {
        let inner = Arc::new(Inner {
            ws_url: ws_url.to_string(),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            subscribers: Mutex::new(Vec::new()),
            link: Mutex::new(Link {
                out: None,
                generation: 0,
            }),
            sessions: Mutex::new(Sessions::default()),
            reconnect_lock: tokio::sync::Mutex::new(()),
            auto_reconnect: AtomicBool::new(true),
            timeout: Mutex::new(DEFAULT_TIMEOUT),
        });
        open_link(&inner).await?;
        Ok(Self { inner })
    }

    /// The websocket URL this connection uses.
    pub fn websocket_url(&self) -> &str {
        &self.inner.ws_url
    }

    /// Whether the websocket is currently down.
    pub fn is_closed(&self) -> bool {
        self.inner.link.lock().unwrap().out.is_none()
    }

    /// Number of the current websocket: 1 for the first, +1 per reconnection.
    pub fn generation(&self) -> u64 {
        self.inner.link.lock().unwrap().generation
    }

    /// Turns automatic reconnection on (default) or off.
    pub fn set_auto_reconnect(&self, on: bool) {
        self.inner.auto_reconnect.store(on, Ordering::SeqCst);
    }

    /// Changes the default reply time limit for subsequent commands.
    pub fn set_timeout(&self, timeout: Duration) {
        *self.inner.timeout.lock().unwrap() = timeout;
    }

    /// Closes the websocket without closing the browser. With auto reconnect on, a new one is
    /// opened right away.
    pub fn disconnect(&self) {
        let out = self.inner.link.lock().unwrap().out.take();
        if let Some(out) = out {
            let _ = out.send(Message::Close(None));
        }
    }

    /// Reopens the websocket now and re-attaches registered targets. Does nothing when the
    /// link is up.
    pub async fn reconnect(&self) -> Result<()> {
        self.ensure_link(true).await
    }

    /// Attaches a flattened session to a target and registers it for re-attachment after a
    /// reconnection. Returns the existing session when the target is already attached.
    pub async fn attach(&self, target_id: &str) -> Result<String> {
        self.ensure_link(false).await?;
        if let Some(s) = self.session_of(target_id) {
            return Ok(s);
        }
        let session = attach_raw(&self.inner, target_id).await?;
        let mut s = self.inner.sessions.lock().unwrap();
        s.by_session.insert(session.clone(), target_id.to_string());
        s.by_target.insert(target_id.to_string(), session.clone());
        Ok(session)
    }

    /// Stops tracking a target (after it was closed).
    pub fn forget_target(&self, target_id: &str) {
        let mut s = self.inner.sessions.lock().unwrap();
        if let Some(session) = s.by_target.remove(target_id) {
            s.by_session.remove(&session);
        }
    }

    /// Current session id of an attached target.
    pub fn session_of(&self, target_id: &str) -> Option<String> {
        self.inner
            .sessions
            .lock()
            .unwrap()
            .by_target
            .get(target_id)
            .cloned()
    }

    /// Sends a typed command, optionally inside a raw session.
    pub async fn send<C: Command>(&self, cmd: &C, session: Option<&str>) -> Result<C::Returns> {
        let params = serde_json::to_value(cmd)?;
        let v = self.call(C::METHOD, params, session).await?;
        Ok(serde_json::from_value(v)?)
    }

    /// Sends a typed command to the session currently attached to `target_id`.
    pub async fn send_target<C: Command>(&self, cmd: &C, target_id: &str) -> Result<C::Returns> {
        let params = serde_json::to_value(cmd)?;
        let v = self.call_target(C::METHOD, params, target_id).await?;
        Ok(serde_json::from_value(v)?)
    }

    /// Sends a command by name with raw JSON params and returns the raw `result`.
    pub async fn call(&self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        let timeout = *self.inner.timeout.lock().unwrap();
        self.call_with_timeout(method, params, session, timeout).await
    }

    /// Like [`Connection::call`], in the session currently attached to `target_id`.
    pub async fn call_target(&self, method: &str, params: Value, target_id: &str) -> Result<Value> {
        self.ensure_link(false).await?;
        let session = self
            .session_of(target_id)
            .ok_or_else(|| Error::NotFound(format!("target {target_id} is not attached")))?;
        let timeout = *self.inner.timeout.lock().unwrap();
        raw_call(&self.inner, method, params, Some(&session), timeout).await
    }

    /// Like [`Connection::call`] with an explicit time limit.
    pub async fn call_with_timeout(
        &self,
        method: &str,
        params: Value,
        session: Option<&str>,
        timeout: Duration,
    ) -> Result<Value> {
        self.ensure_link(false).await?;
        raw_call(&self.inner, method, params, session, timeout).await
    }

    /// Subscribes to one typed event. `session = None` receives it from every session.
    ///
    /// Subscribe before sending the command that triggers the event to avoid missing it.
    pub fn subscribe<E: Event>(&self, session: Option<&str>) -> EventStream<E> {
        let scope = session.map_or(Scope::All, |s| Scope::Session(s.to_string()));
        let rx = self.add_subscriber(MethodFilter::Exact(E::METHOD.to_string()), scope);
        EventStream {
            rx,
            _marker: PhantomData,
        }
    }

    /// Subscribes to one typed event of a target, across reconnections.
    pub fn subscribe_target<E: Event>(&self, target_id: &str) -> EventStream<E> {
        let rx = self.add_subscriber(
            MethodFilter::Exact(E::METHOD.to_string()),
            Scope::Target(target_id.to_string()),
        );
        EventStream {
            rx,
            _marker: PhantomData,
        }
    }

    /// Subscribes to raw events. `method` may be an exact name (`"Page.loadEventFired"`),
    /// a domain prefix ending in a dot (`"Network."`) or `None` for everything.
    pub fn subscribe_raw(&self, method: Option<&str>, session: Option<&str>) -> RawEventStream {
        let scope = session.map_or(Scope::All, |s| Scope::Session(s.to_string()));
        RawEventStream {
            rx: self.add_subscriber(method_filter(method), scope),
        }
    }

    /// Raw events of a target, across reconnections.
    pub fn subscribe_raw_target(&self, method: Option<&str>, target_id: &str) -> RawEventStream {
        RawEventStream {
            rx: self.add_subscriber(method_filter(method), Scope::Target(target_id.to_string())),
        }
    }

    fn add_subscriber(&self, method: MethodFilter, scope: Scope) -> mpsc::UnboundedReceiver<Arc<RawEvent>> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.inner
            .subscribers
            .lock()
            .unwrap()
            .push(Subscriber { method, scope, tx });
        rx
    }

    /// Makes sure the websocket is up, reconnecting if allowed (or `force`d).
    async fn ensure_link(&self, force: bool) -> Result<()> {
        if !self.is_closed() {
            return Ok(());
        }
        if !force && !self.inner.auto_reconnect.load(Ordering::SeqCst) {
            return Err(Error::Disconnected);
        }
        let _guard = self.inner.reconnect_lock.lock().await;
        if !self.is_closed() {
            return Ok(());
        }
        let mut last_err = Error::Disconnected;
        let mut opened = false;
        for attempt in 0..RECONNECT_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(250 * u64::from(attempt))).await;
            }
            match open_link(&self.inner).await {
                Ok(()) => {
                    opened = true;
                    break;
                }
                Err(e) => last_err = e,
            }
        }
        if !opened {
            tracing::warn!("reconnection to {} failed: {last_err}", self.inner.ws_url);
            // The browser is gone: end every event stream.
            self.inner.subscribers.lock().unwrap().clear();
            return Err(Error::Disconnected);
        }

        let targets: Vec<String> = {
            let mut s = self.inner.sessions.lock().unwrap();
            s.by_session.clear();
            s.by_target.drain().map(|(t, _)| t).collect()
        };
        for target in targets {
            match attach_raw(&self.inner, &target).await {
                Ok(session) => {
                    let mut s = self.inner.sessions.lock().unwrap();
                    s.by_session.insert(session.clone(), target.clone());
                    s.by_target.insert(target, session);
                }
                Err(e) => tracing::debug!("target {target} not re-attached: {e}"),
            }
        }
        let generation = self.generation();
        tracing::debug!("reconnected to {} (generation {generation})", self.inner.ws_url);
        self.inner.dispatch(json!({
            "method": RECONNECTED_EVENT,
            "params": { "generation": generation },
        }));
        Ok(())
    }
}

fn method_filter(method: Option<&str>) -> MethodFilter {
    match method {
        None => MethodFilter::Any,
        Some(m) if m.ends_with('.') => MethodFilter::Prefix(m.to_string()),
        Some(m) => MethodFilter::Exact(m.to_string()),
    }
}

async fn attach_raw(inner: &Arc<Inner>, target_id: &str) -> Result<String> {
    let timeout = *inner.timeout.lock().unwrap();
    let r = raw_call(
        inner,
        "Target.attachToTarget",
        json!({ "targetId": target_id, "flatten": true }),
        None,
        timeout,
    )
    .await?;
    r.get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| Error::NotFound(format!("no sessionId attaching to {target_id}")))
}

/// Sends on the current link without reconnecting.
async fn raw_call(
    inner: &Arc<Inner>,
    method: &str,
    params: Value,
    session: Option<&str>,
    timeout: Duration,
) -> Result<Value> {
    let id = inner.next_id.fetch_add(1, Ordering::SeqCst);
    let mut msg = json!({ "id": id, "method": method, "params": params });
    if let Some(s) = session {
        msg["sessionId"] = Value::String(s.to_string());
    }
    let (tx, rx) = oneshot::channel();
    {
        let link = inner.link.lock().unwrap();
        let Some(out) = &link.out else {
            return Err(Error::Disconnected);
        };
        inner.pending.lock().unwrap().insert(
            id,
            PendingCall {
                method: method.to_string(),
                generation: link.generation,
                tx,
            },
        );
        if out.send(Message::Text(msg.to_string().into())).is_err() {
            inner.pending.lock().unwrap().remove(&id);
            return Err(Error::Disconnected);
        }
    }
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => Err(Error::Disconnected),
        Err(_) => {
            inner.pending.lock().unwrap().remove(&id);
            Err(Error::Timeout(method.to_string()))
        }
    }
}

/// Opens a websocket and installs it as the current link (next generation).
///
/// Boxed because the reader task it spawns can start a reconnection, which calls this function
/// again; the explicit `Send` future breaks that cycle for the compiler.
fn open_link(inner: &Arc<Inner>) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
    Box::pin(open_link_inner(inner))
}

async fn open_link_inner(inner: &Arc<Inner>) -> Result<()> {
    let config = WebSocketConfig::default()
        .max_message_size(None)
        .max_frame_size(None);
    let (ws, _) = tokio_tungstenite::connect_async_with_config(&inner.ws_url, Some(config), false).await?;
    let (mut sink, mut stream) = ws.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();

    let generation = {
        let mut link = inner.link.lock().unwrap();
        link.generation += 1;
        link.out = Some(out_tx);
        link.generation
    };

    tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let close = matches!(msg, Message::Close(_));
            if sink.send(msg).await.is_err() || close {
                break;
            }
        }
        let _ = sink.close().await;
    });

    let reader: Weak<Inner> = Arc::downgrade(inner);
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
            inner.link_lost(generation);
            if inner.auto_reconnect.load(Ordering::SeqCst) {
                let conn = Connection { inner };
                tokio::spawn(async move {
                    let _ = conn.ensure_link(false).await;
                });
            }
        }
    });
    Ok(())
}

impl Inner {
    fn dispatch(&self, v: Value) {
        if let Some(id) = v.get("id").and_then(Value::as_u64) {
            let Some(call) = self.pending.lock().unwrap().remove(&id) else {
                return;
            };
            let reply = match v.get("error") {
                Some(err) => Err(Error::Protocol {
                    method: call.method,
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
            let _ = call.tx.send(reply);
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
        let target = ev
            .session_id
            .as_ref()
            .and_then(|s| self.sessions.lock().unwrap().by_session.get(s).cloned());
        self.subscribers
            .lock()
            .unwrap()
            .retain(|s| !s.wants(&ev, target.as_deref()) || s.tx.send(ev.clone()).is_ok());
    }

    /// The websocket of `generation` closed: fail its pending calls and mark the link down.
    fn link_lost(&self, generation: u64) {
        {
            let mut link = self.link.lock().unwrap();
            if link.generation == generation {
                link.out = None;
            }
        }
        let failed: Vec<PendingCall> = {
            let mut pending = self.pending.lock().unwrap();
            let ids: Vec<u64> = pending
                .iter()
                .filter(|(_, c)| c.generation == generation)
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter().filter_map(|id| pending.remove(&id)).collect()
        };
        for c in failed {
            let _ = c.tx.send(Err(Error::Disconnected));
        }
        if !self.auto_reconnect.load(Ordering::SeqCst) {
            self.subscribers.lock().unwrap().clear();
        }
    }
}

/// Stream of one typed event. Ends when the connection is closed for good.
pub struct EventStream<E> {
    rx: mpsc::UnboundedReceiver<Arc<RawEvent>>,
    _marker: PhantomData<E>,
}

impl<E: Event> EventStream<E> {
    /// Next event; `None` once the connection is closed for good. Events that fail to
    /// deserialize are logged and skipped.
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

/// Stream of raw events. Ends when the connection is closed for good.
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
