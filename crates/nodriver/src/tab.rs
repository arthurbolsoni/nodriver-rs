//! A page target with its own session. Mirrors nodriver's `core/tab.py`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use nodriver_cdp::{Command, Event, input, page, runtime, target};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::connection::{Connection, EventStream, RawEventStream};
use crate::element::Element;
use crate::error::{Error, Result};

/// Default time limit for `navigate`, `select` and `find`.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(10);
const LOAD_TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(100);
/// Object group for handles created by this crate (`Runtime.releaseObjectGroup` frees them).
pub const OBJECT_GROUP: &str = "nodriver";

const FIND_JS: &str = include_str!("js/find.js");

struct Inner {
    conn: Connection,
    target_id: String,
    /// Connection generation in which `Page.enable` was sent (0 = never). A reconnection
    /// creates a new session, where `Page` is disabled again.
    page_enabled_in: AtomicU64,
}

/// A tab (page target). Cheap to clone; clones share the session. Commands go to the session
/// currently attached to the target, so a tab keeps working after the connection reconnects.
#[derive(Clone)]
pub struct Tab {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Tab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tab")
            .field("target_id", &self.inner.target_id)
            .finish()
    }
}

impl Tab {
    pub(crate) fn new(conn: Connection, target_id: String) -> Self {
        Self {
            inner: Arc::new(Inner {
                conn,
                target_id,
                page_enabled_in: AtomicU64::new(0),
            }),
        }
    }

    pub fn target_id(&self) -> &str {
        &self.inner.target_id
    }

    /// Session currently attached to this tab (changes after a reconnection).
    pub fn session_id(&self) -> Option<String> {
        self.inner.conn.session_of(&self.inner.target_id)
    }

    pub fn connection(&self) -> &Connection {
        &self.inner.conn
    }

    /// Sends a typed command in this tab's session.
    pub async fn send<C: Command>(&self, cmd: &C) -> Result<C::Returns> {
        self.inner.conn.send_target(cmd, &self.inner.target_id).await
    }

    /// Sends a command by name with raw JSON params in this tab's session.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.inner
            .conn
            .call_target(method, params, &self.inner.target_id)
            .await
    }

    /// Typed events of this tab. The domain must be enabled for most events.
    pub fn subscribe<E: Event>(&self) -> EventStream<E> {
        self.inner.conn.subscribe_target(&self.inner.target_id)
    }

    /// Raw events of this tab; `method` as in [`Connection::subscribe_raw`].
    pub fn subscribe_raw(&self, method: Option<&str>) -> RawEventStream {
        self.inner
            .conn
            .subscribe_raw_target(method, &self.inner.target_id)
    }

    async fn ensure_page_enabled(&self) -> Result<()> {
        let generation = self.inner.conn.generation();
        if self.inner.page_enabled_in.load(Ordering::SeqCst) != generation {
            self.send(&page::EnableParams::new()).await?;
            self.inner.page_enabled_in.store(generation, Ordering::SeqCst);
        }
        Ok(())
    }

    /// Navigates and waits for the load event of the new document (same-document navigations
    /// return right away).
    pub async fn navigate(&self, url: &str) -> Result<()> {
        self.ensure_page_enabled().await?;
        let mut loaded = self.subscribe::<page::LoadEventFiredEvent>();
        let r = self.send(&page::NavigateParams::new(url)).await?;
        if let Some(err) = r.error_text {
            return Err(Error::Navigation(format!("{url}: {err}")));
        }
        if r.loader_id.is_none() {
            return Ok(());
        }
        match tokio::time::timeout(LOAD_TIMEOUT, loaded.next()).await {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(Error::Disconnected),
            Err(_) => Err(Error::Timeout(format!("load event of {url}"))),
        }
    }

    /// Reloads and waits for the load event.
    pub async fn reload(&self) -> Result<()> {
        self.ensure_page_enabled().await?;
        let mut loaded = self.subscribe::<page::LoadEventFiredEvent>();
        self.send(&page::ReloadParams::new()).await?;
        match tokio::time::timeout(LOAD_TIMEOUT, loaded.next()).await {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(Error::Disconnected),
            Err(_) => Err(Error::Timeout("load event after reload".into())),
        }
    }

    /// Waits until `document.readyState` is `"complete"`.
    pub async fn wait_for_ready(&self, timeout: Duration) -> Result<()> {
        self.wait_until(timeout, "document.readyState === 'complete'", "document ready")
            .await
    }

    /// Polls a JavaScript expression until it is truthy.
    pub async fn wait_until(&self, timeout: Duration, expression: &str, what: &str) -> Result<()> {
        let start = Instant::now();
        loop {
            match self.evaluate(expression).await {
                Ok(v) if truthy(&v) => return Ok(()),
                Ok(_) | Err(Error::Js(_)) => {}
                Err(e) => return Err(e),
            }
            if start.elapsed() > timeout {
                return Err(Error::Timeout(what.to_string()));
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Evaluates an expression in the page (promises are awaited) and returns its JSON value.
    /// `undefined` becomes `Value::Null`.
    pub async fn evaluate(&self, expression: &str) -> Result<Value> {
        let mut p = runtime::EvaluateParams::new(expression);
        p.await_promise = Some(true);
        p.return_by_value = Some(true);
        p.user_gesture = Some(true);
        let r = self.send(&p).await?;
        if let Some(ex) = r.exception_details {
            return Err(Error::Js(exception_text(&ex)));
        }
        Ok(r.result.value.unwrap_or(Value::Null))
    }

    /// Evaluates and deserializes the result.
    pub async fn evaluate_as<T: DeserializeOwned>(&self, expression: &str) -> Result<T> {
        Ok(serde_json::from_value(self.evaluate(expression).await?)?)
    }

    /// Evaluates an expression that yields a DOM element; `None` for `null`/`undefined`.
    pub async fn evaluate_element(&self, expression: &str) -> Result<Option<Element>> {
        let obj = self.evaluate_handle(expression).await?;
        Ok(obj.object_id.map(|id| Element::new(self.clone(), id)))
    }

    /// Evaluates and returns the remote object (no value copy).
    pub async fn evaluate_handle(&self, expression: &str) -> Result<runtime::RemoteObject> {
        let mut p = runtime::EvaluateParams::new(expression);
        p.await_promise = Some(true);
        p.return_by_value = Some(false);
        p.user_gesture = Some(true);
        p.object_group = Some(OBJECT_GROUP.into());
        let r = self.send(&p).await?;
        if let Some(ex) = r.exception_details {
            return Err(Error::Js(exception_text(&ex)));
        }
        Ok(r.result)
    }

    /// Elements of an array-valued expression.
    pub async fn evaluate_elements(&self, expression: &str) -> Result<Vec<Element>> {
        let arr = self.evaluate_handle(expression).await?;
        let Some(id) = arr.object_id else {
            return Ok(Vec::new());
        };
        array_elements(self, &id).await
    }

    /// First element matching a CSS selector, without waiting.
    pub async fn query(&self, selector: &str) -> Result<Option<Element>> {
        self.evaluate_element(&format!("document.querySelector({})", js_str(selector)))
            .await
    }

    /// Every element matching a CSS selector, without waiting.
    pub async fn query_all(&self, selector: &str) -> Result<Vec<Element>> {
        self.evaluate_elements(&format!(
            "Array.from(document.querySelectorAll({}))",
            js_str(selector)
        ))
        .await
    }

    /// Waits up to [`DEFAULT_WAIT`] for an element matching a CSS selector.
    pub async fn select(&self, selector: &str) -> Result<Element> {
        self.select_timeout(selector, DEFAULT_WAIT).await
    }

    pub async fn select_timeout(&self, selector: &str, timeout: Duration) -> Result<Element> {
        let start = Instant::now();
        loop {
            if let Some(el) = self.query(selector).await? {
                return Ok(el);
            }
            if start.elapsed() > timeout {
                return Err(Error::NotFound(format!("selector {selector}")));
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Waits up to [`DEFAULT_WAIT`] for an element containing `text` (text, aria-label,
    /// placeholder, title, value or alt). With `best_match`, the visible element whose text
    /// length is closest to `text` wins; otherwise the first in document order.
    pub async fn find(&self, text: &str, best_match: bool) -> Result<Element> {
        self.find_timeout(text, best_match, DEFAULT_WAIT).await
    }

    pub async fn find_timeout(&self, text: &str, best_match: bool, timeout: Duration) -> Result<Element> {
        let expr = format!("({FIND_JS})({}, {best_match}, false)", js_str(text));
        let start = Instant::now();
        loop {
            if let Some(el) = self.evaluate_element(&expr).await? {
                return Ok(el);
            }
            if start.elapsed() > timeout {
                return Err(Error::NotFound(format!("text {text:?}")));
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Every element containing `text`, without waiting.
    pub async fn find_all(&self, text: &str) -> Result<Vec<Element>> {
        self.evaluate_elements(&format!("({FIND_JS})({}, false, true)", js_str(text)))
            .await
    }

    pub async fn url(&self) -> Result<String> {
        self.evaluate_as("location.href").await
    }

    pub async fn title(&self) -> Result<String> {
        self.evaluate_as("document.title").await
    }

    /// Focuses this tab in the browser window.
    pub async fn activate(&self) -> Result<()> {
        self.inner
            .conn
            .send(
                &target::ActivateTargetParams::new(self.inner.target_id.clone()),
                None,
            )
            .await?;
        Ok(())
    }

    pub async fn close(&self) -> Result<()> {
        self.inner
            .conn
            .send(
                &target::CloseTargetParams::new(self.inner.target_id.clone()),
                None,
            )
            .await?;
        self.inner.conn.forget_target(&self.inner.target_id);
        Ok(())
    }

    /// Mouse move to viewport coordinates (CSS pixels).
    pub async fn mouse_move(&self, x: f64, y: f64) -> Result<()> {
        let p = input::DispatchMouseEventParams::new(input::DispatchMouseEventType::MouseMoved, x, y);
        self.send(&p).await?;
        Ok(())
    }

    /// Left click at viewport coordinates: move, press, release.
    pub async fn mouse_click(&self, x: f64, y: f64) -> Result<()> {
        self.mouse_move(x, y).await?;
        for kind in [
            input::DispatchMouseEventType::MousePressed,
            input::DispatchMouseEventType::MouseReleased,
        ] {
            let mut p = input::DispatchMouseEventParams::new(kind, x, y);
            p.button = Some(input::MouseButton::Left);
            p.buttons = Some(1);
            p.click_count = Some(1);
            self.send(&p).await?;
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        Ok(())
    }

    /// Types text into the focused element as `char` key events (nodriver's `send_keys`).
    pub async fn type_text(&self, text: &str) -> Result<()> {
        for c in text.chars() {
            let mut p = input::DispatchKeyEventParams::new(input::DispatchKeyEventType::Char);
            p.text = Some(c.to_string());
            self.send(&p).await?;
        }
        Ok(())
    }

    /// Inserts text into the focused element in one step (`Input.insertText`).
    pub async fn insert_text(&self, text: &str) -> Result<()> {
        self.send(&input::InsertTextParams::new(text)).await?;
        Ok(())
    }

    /// Presses and releases a named key: `Enter`, `Tab`, `Escape`, `Backspace`, `Delete`,
    /// `ArrowUp`/`Down`/`Left`/`Right`, `Home`, `End`, `PageUp`, `PageDown`, `Space`, or a
    /// single character.
    pub async fn press_key(&self, key: &str) -> Result<()> {
        let (key_name, code, vk, text) =
            key_info(key).ok_or_else(|| Error::NotFound(format!("key {key}")))?;
        let mut down = input::DispatchKeyEventParams::new(if text.is_some() {
            input::DispatchKeyEventType::KeyDown
        } else {
            input::DispatchKeyEventType::RawKeyDown
        });
        down.key = Some(key_name.clone());
        down.code = Some(code.clone());
        down.windows_virtual_key_code = Some(vk);
        down.native_virtual_key_code = Some(vk);
        down.text = text.clone();
        down.unmodified_text = text;
        self.send(&down).await?;
        let mut up = input::DispatchKeyEventParams::new(input::DispatchKeyEventType::KeyUp);
        up.key = Some(key_name);
        up.code = Some(code);
        up.windows_virtual_key_code = Some(vk);
        up.native_virtual_key_code = Some(vk);
        self.send(&up).await?;
        Ok(())
    }

    /// PNG screenshot of the viewport.
    pub async fn screenshot_png(&self) -> Result<Vec<u8>> {
        let r = self.send(&page::CaptureScreenshotParams::new()).await?;
        crate::util::base64_decode(&r.data).ok_or_else(|| Error::Js("invalid base64 screenshot".into()))
    }

    /// Frees element handles created by this crate in this tab.
    pub async fn release_handles(&self) -> Result<()> {
        self.send(&runtime::ReleaseObjectGroupParams::new(OBJECT_GROUP))
            .await?;
        Ok(())
    }
}

pub(crate) async fn array_elements(tab: &Tab, array_id: &str) -> Result<Vec<Element>> {
    let mut p = runtime::GetPropertiesParams::new(array_id.to_string());
    p.own_properties = Some(true);
    let props = tab.send(&p).await?;
    let mut items: Vec<(usize, Element)> = props
        .result
        .into_iter()
        .filter_map(|d| {
            let idx = d.name.parse::<usize>().ok()?;
            let id = d.value?.object_id?;
            Some((idx, Element::new(tab.clone(), id)))
        })
        .collect();
    items.sort_by_key(|(i, _)| *i);
    Ok(items.into_iter().map(|(_, e)| e).collect())
}

pub(crate) fn exception_text(ex: &runtime::ExceptionDetails) -> String {
    ex.exception
        .as_ref()
        .and_then(|e| e.description.clone())
        .unwrap_or_else(|| ex.text.clone())
}

/// JavaScript string literal.
pub(crate) fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// (key, code, windows virtual key code, text)
fn key_info(key: &str) -> Option<(String, String, i64, Option<String>)> {
    let named = |k: &str, code: &str, vk: i64, text: Option<&str>| {
        Some((k.to_string(), code.to_string(), vk, text.map(str::to_string)))
    };
    match key {
        "Enter" => named("Enter", "Enter", 13, Some("\r")),
        "Tab" => named("Tab", "Tab", 9, None),
        "Escape" | "Esc" => named("Escape", "Escape", 27, None),
        "Backspace" => named("Backspace", "Backspace", 8, None),
        "Delete" => named("Delete", "Delete", 46, None),
        "ArrowUp" => named("ArrowUp", "ArrowUp", 38, None),
        "ArrowDown" => named("ArrowDown", "ArrowDown", 40, None),
        "ArrowLeft" => named("ArrowLeft", "ArrowLeft", 37, None),
        "ArrowRight" => named("ArrowRight", "ArrowRight", 39, None),
        "Home" => named("Home", "Home", 36, None),
        "End" => named("End", "End", 35, None),
        "PageUp" => named("PageUp", "PageUp", 33, None),
        "PageDown" => named("PageDown", "PageDown", 34, None),
        "Space" | " " => named(" ", "Space", 32, Some(" ")),
        _ => {
            let mut cs = key.chars();
            let c = cs.next()?;
            if cs.next().is_some() {
                return None;
            }
            let upper = c.to_ascii_uppercase();
            let (code, vk) = if upper.is_ascii_alphabetic() {
                (format!("Key{upper}"), upper as i64)
            } else if c.is_ascii_digit() {
                (format!("Digit{c}"), c as i64)
            } else {
                (String::new(), 0)
            };
            Some((c.to_string(), code, vk, Some(c.to_string())))
        }
    }
}
