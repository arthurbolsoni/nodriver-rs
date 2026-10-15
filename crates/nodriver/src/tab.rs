//! A page target with its own session. Mirrors nodriver's `core/tab.py`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use nodriver_cdp::{Command, Event, browser as cdp_browser, dom_storage, input, page, runtime, target};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::connection::{Connection, EventStream, RawEventStream};
use crate::element::Element;
use crate::error::{Error, Result};

/// Default time limit for `navigate`, `select` and `find`.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(10);
const LOAD_TIMEOUT: Duration = Duration::from_secs(30);
const SAME_DOC_TIMEOUT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(100);
/// Speed of synthesized scroll gestures in pixels per second (nodriver's value).
const SCROLL_SPEED: i64 = 7777;
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
        let mut same_doc = self.subscribe::<page::NavigatedWithinDocumentEvent>();
        let r = self.send(&page::NavigateParams::new(url)).await?;
        if let Some(err) = r.error_text {
            return Err(Error::Navigation(format!("{url}: {err}")));
        }
        if r.loader_id.is_none() {
            // Same-document: the history entry commits a moment after the reply.
            let _ = tokio::time::timeout(SAME_DOC_TIMEOUT, async {
                while let Some(ev) = same_doc.next().await {
                    if ev.frame_id == r.frame_id {
                        break;
                    }
                }
            })
            .await;
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
            p.unmodified_text = Some(c.to_string());
            // Without `key` the page sees `keypress` with an empty `event.key`.
            p.key = Some(c.to_string());
            self.send(&p).await?;
        }
        Ok(())
    }

    /// Inserts text into the focused element in one step (`Input.insertText`).
    pub async fn insert_text(&self, text: &str) -> Result<()> {
        self.send(&input::InsertTextParams::new(text)).await?;
        Ok(())
    }

    /// Presses and releases a key, optionally with modifiers: `Enter`, `Tab`, `Escape`,
    /// `Backspace`, `Delete`, `Insert`, `ArrowUp`/`Down`/`Left`/`Right`, `Home`, `End`, `PageUp`,
    /// `PageDown`, `Space`, `F1`..`F12`, or a single character; modifiers are joined with `+`
    /// (`Control+a`, `Shift+Tab`, `Control+Shift+K`; `Ctrl`, `Alt`, `Shift`, `Meta`).
    pub async fn press_key(&self, key: &str) -> Result<()> {
        let (mods, key) = split_modifiers(key);
        let (key_name, code, vk, text) =
            key_info(key).ok_or_else(|| Error::NotFound(format!("key {key}")))?;
        let bits: i64 = mods.iter().map(|m| m.bit).sum();
        let mut held = 0;
        for m in &mods {
            held |= m.bit;
            let mut p = input::DispatchKeyEventParams::new(input::DispatchKeyEventType::RawKeyDown);
            p.key = Some(m.key.into());
            p.code = Some(m.code.into());
            p.windows_virtual_key_code = Some(m.vk);
            p.native_virtual_key_code = Some(m.vk);
            p.modifiers = Some(held);
            self.send(&p).await?;
        }
        // With Ctrl, Alt or Meta held the key is a shortcut and inserts no text.
        let text = if bits & !SHIFT != 0 {
            None
        } else if bits & SHIFT != 0 {
            text.map(|t| t.to_uppercase())
        } else {
            text
        };
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
        down.modifiers = Some(bits);
        self.send(&down).await?;
        let mut up = input::DispatchKeyEventParams::new(input::DispatchKeyEventType::KeyUp);
        up.key = Some(key_name);
        up.code = Some(code);
        up.windows_virtual_key_code = Some(vk);
        up.native_virtual_key_code = Some(vk);
        up.modifiers = Some(bits);
        self.send(&up).await?;
        for m in mods.iter().rev() {
            held &= !m.bit;
            let mut p = input::DispatchKeyEventParams::new(input::DispatchKeyEventType::KeyUp);
            p.key = Some(m.key.into());
            p.code = Some(m.code.into());
            p.windows_virtual_key_code = Some(m.vk);
            p.native_virtual_key_code = Some(m.vk);
            p.modifiers = Some(held);
            self.send(&p).await?;
        }
        Ok(())
    }

    /// PNG screenshot of the viewport.
    pub async fn screenshot_png(&self) -> Result<Vec<u8>> {
        self.screenshot(ImageFormat::Png, false).await
    }

    /// Screenshot of the viewport, or of the whole page with `full_page` (`captureBeyondViewport`
    /// with the content size as clip).
    pub async fn screenshot(&self, format: ImageFormat, full_page: bool) -> Result<Vec<u8>> {
        let mut p = page::CaptureScreenshotParams::new();
        p.format = Some(format.cdp());
        if full_page {
            let m = self.send(&page::GetLayoutMetricsParams::new()).await?;
            let size = m.css_content_size;
            p.clip = Some(page::Viewport {
                x: 0.0,
                y: 0.0,
                width: size.width.ceil(),
                height: size.height.ceil(),
                scale: 1.0,
            });
            p.capture_beyond_viewport = Some(true);
        }
        let r = self.send(&p).await?;
        crate::util::base64_decode(&r.data).ok_or_else(|| Error::Js("invalid base64 screenshot".into()))
    }

    /// Writes a screenshot to `path`; JPEG for `.jpg`/`.jpeg`, PNG otherwise (nodriver's
    /// `save_screenshot`).
    pub async fn save_screenshot(&self, path: impl AsRef<Path>, full_page: bool) -> Result<()> {
        let path = path.as_ref();
        let data = self.screenshot(ImageFormat::from_path(path), full_page).await?;
        write_file(path, &data)
    }

    /// HTML of the document, doctype included (nodriver's `get_content`).
    pub async fn content(&self) -> Result<String> {
        let doc = self.call("DOM.getDocument", json!({ "depth": 0 })).await?;
        let id = doc["root"]["backendNodeId"].clone();
        let r = self
            .call("DOM.getOuterHTML", json!({ "backendNodeId": id }))
            .await?;
        Ok(r["outerHTML"].as_str().unwrap_or_default().to_string())
    }

    /// Goes back one entry in the session history and waits for the page (load event,
    /// same-document navigation or back/forward cache restore).
    pub async fn back(&self) -> Result<()> {
        self.history_step(-1).await
    }

    /// Goes forward one entry in the session history; see [`Tab::back`].
    pub async fn forward(&self) -> Result<()> {
        self.history_step(1).await
    }

    async fn history_step(&self, delta: i64) -> Result<()> {
        self.ensure_page_enabled().await?;
        let h = self.send(&page::GetNavigationHistoryParams::new()).await?;
        let entry = usize::try_from(h.current_index + delta)
            .ok()
            .and_then(|i| h.entries.get(i))
            .ok_or_else(|| {
                Error::NotFound(if delta < 0 {
                    "no previous history entry".into()
                } else {
                    "no next history entry".into()
                })
            })?;
        let main = self.frame_tree().await?.frame.id;
        let mut events = self.subscribe_raw(Some("Page."));
        self.send(&page::NavigateToHistoryEntryParams::new(entry.id))
            .await?;
        let done = async {
            while let Some(ev) = events.next().await {
                let p = &ev.params;
                let hit = match ev.method.as_str() {
                    "Page.loadEventFired" => true,
                    "Page.navigatedWithinDocument" => p["frameId"] == json!(main),
                    "Page.frameNavigated" => {
                        p["frame"]["parentId"].is_null() && p["type"] == json!("BackForwardCacheRestore")
                    }
                    _ => false,
                };
                if hit {
                    return Ok(());
                }
            }
            Err(Error::Disconnected)
        };
        tokio::time::timeout(LOAD_TIMEOUT, done)
            .await
            .map_err(|_| Error::Timeout("history navigation".into()))?
    }

    /// Reloads with options and waits for the load event (nodriver's `reload(ignore_cache,
    /// script_to_evaluate_on_load)`).
    pub async fn reload_with(
        &self,
        ignore_cache: bool,
        script_to_evaluate_on_load: Option<&str>,
    ) -> Result<()> {
        self.ensure_page_enabled().await?;
        let mut loaded = self.subscribe::<page::LoadEventFiredEvent>();
        let mut p = page::ReloadParams::new();
        p.ignore_cache = Some(ignore_cache);
        p.script_to_evaluate_on_load = script_to_evaluate_on_load.map(str::to_string);
        self.send(&p).await?;
        match tokio::time::timeout(LOAD_TIMEOUT, loaded.next()).await {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(Error::Disconnected),
            Err(_) => Err(Error::Timeout("load event after reload".into())),
        }
    }

    /// Scrolls the page down by `percent` of the window height with a synthesized scroll gesture
    /// (`Input.synthesizeScrollGesture`), as a user would with the wheel or touchpad: 25 is a
    /// quarter of the window, 100 a whole window, 1000 ten windows. Returns when the gesture ends.
    pub async fn scroll_down(&self, percent: f64) -> Result<()> {
        let h = self.window_height().await?;
        self.scroll_gesture(0.0, 0.0, 0.0, h * percent / 100.0).await
    }

    /// Scrolls the page up by `percent` of the window height; see [`Tab::scroll_down`].
    pub async fn scroll_up(&self, percent: f64) -> Result<()> {
        let h = self.window_height().await?;
        self.scroll_gesture(0.0, 0.0, 0.0, -h * percent / 100.0).await
    }

    /// Scroll gesture starting at viewport point `(x, y)`: positive `dx`/`dy` scroll right/down.
    /// The element under the point scrolls first; the rest chains to its scrollable ancestors.
    pub async fn scroll_gesture(&self, x: f64, y: f64, dx: f64, dy: f64) -> Result<()> {
        let mut p = input::SynthesizeScrollGestureParams::new(x, y);
        p.x_distance = Some(-dx);
        p.y_distance = Some(-dy);
        p.x_overscroll = Some(0.0);
        p.y_overscroll = Some(0.0);
        p.prevent_fling = Some(true);
        p.repeat_delay_ms = Some(0);
        p.speed = Some(SCROLL_SPEED);
        self.send(&p).await?;
        Ok(())
    }

    /// Whether the page is scrolled to the bottom (nodriver's `scroll_bottom_reached`).
    pub async fn scroll_bottom_reached(&self) -> Result<bool> {
        self.evaluate_as(
            "(() => { const e = document.scrollingElement || document.documentElement; \
             return Math.ceil(e.scrollTop + e.clientHeight) >= e.scrollHeight - 1; })()",
        )
        .await
    }

    async fn window_height(&self) -> Result<f64> {
        match self.get_window().await {
            Ok((_, b)) if b.height.is_some_and(|h| h > 0) => Ok(b.height.unwrap() as f64),
            _ => self.evaluate_as("window.innerHeight").await,
        }
    }

    /// Window that holds this tab and its bounds (`Browser.getWindowForTarget`).
    pub async fn get_window(&self) -> Result<(cdp_browser::WindowID, cdp_browser::Bounds)> {
        let r = self.send(&cdp_browser::GetWindowForTargetParams::new()).await?;
        Ok((r.window_id, r.bounds))
    }

    /// Puts the window in the normal state and moves/resizes it (nodriver's `set_window_size`).
    pub async fn set_window_bounds(&self, left: i64, top: i64, width: i64, height: i64) -> Result<()> {
        let (id, b) = self.get_window().await?;
        if b.window_state
            .is_some_and(|s| s != cdp_browser::WindowState::Normal)
        {
            self.set_bounds(id, normal_bounds()).await?;
        }
        self.set_bounds(
            id,
            cdp_browser::Bounds {
                left: Some(left),
                top: Some(top),
                width: Some(width),
                height: Some(height),
                window_state: Some(cdp_browser::WindowState::Normal),
            },
        )
        .await
    }

    /// Normal, minimized, maximized or fullscreen. The other states are reached from normal, as
    /// in nodriver.
    pub async fn set_window_state(&self, state: cdp_browser::WindowState) -> Result<()> {
        let (id, b) = self.get_window().await?;
        if b.window_state.as_ref() == Some(&state) {
            return Ok(());
        }
        if state != cdp_browser::WindowState::Normal
            && b.window_state
                .is_some_and(|s| s != cdp_browser::WindowState::Normal)
        {
            self.set_bounds(id, normal_bounds()).await?;
        }
        let bounds = cdp_browser::Bounds {
            left: None,
            top: None,
            width: None,
            height: None,
            window_state: Some(state),
        };
        self.set_bounds(id, bounds).await
    }

    async fn set_bounds(&self, id: cdp_browser::WindowID, bounds: cdp_browser::Bounds) -> Result<()> {
        self.send(&cdp_browser::SetWindowBoundsParams::new(id, bounds))
            .await?;
        Ok(())
    }

    pub async fn maximize(&self) -> Result<()> {
        self.set_window_state(cdp_browser::WindowState::Maximized).await
    }

    pub async fn minimize(&self) -> Result<()> {
        self.set_window_state(cdp_browser::WindowState::Minimized).await
    }

    pub async fn fullscreen(&self) -> Result<()> {
        self.set_window_state(cdp_browser::WindowState::Fullscreen).await
    }

    /// Back to the normal window state (nodriver's `medimize`).
    pub async fn normal(&self) -> Result<()> {
        self.set_window_state(cdp_browser::WindowState::Normal).await
    }

    /// Presses the left button at `from`, moves to `to` in `steps` moves (at least 1) with the
    /// button held, and releases (nodriver's `mouse_drag`).
    pub async fn mouse_drag(&self, from: (f64, f64), to: (f64, f64), steps: u32) -> Result<()> {
        let steps = steps.max(1);
        self.mouse_move(from.0, from.1).await?;
        let held = |kind, x, y| {
            let mut p = input::DispatchMouseEventParams::new(kind, x, y);
            p.button = Some(input::MouseButton::Left);
            p.buttons = Some(1);
            p
        };
        let mut down = held(input::DispatchMouseEventType::MousePressed, from.0, from.1);
        down.click_count = Some(1);
        self.send(&down).await?;
        for i in 1..=steps {
            let t = i as f64 / steps as f64;
            let (x, y) = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
            self.send(&held(input::DispatchMouseEventType::MouseMoved, x, y))
                .await?;
        }
        let mut up = held(input::DispatchMouseEventType::MouseReleased, to.0, to.1);
        up.buttons = Some(0);
        up.click_count = Some(1);
        self.send(&up).await?;
        Ok(())
    }

    /// Click with any button and modifiers (Alt=1, Ctrl=2, Meta=4, Shift=8), as nodriver's
    /// `mouse_click(x, y, button, modifiers)`.
    pub async fn mouse_click_with(
        &self,
        x: f64,
        y: f64,
        button: input::MouseButton,
        modifiers: i64,
    ) -> Result<()> {
        let bits = match button {
            input::MouseButton::Left => 1,
            input::MouseButton::Right => 2,
            input::MouseButton::Middle => 4,
            input::MouseButton::Back => 8,
            input::MouseButton::Forward => 16,
            _ => 0,
        };
        self.mouse_move(x, y).await?;
        for (kind, buttons) in [
            (input::DispatchMouseEventType::MousePressed, bits),
            (input::DispatchMouseEventType::MouseReleased, 0),
        ] {
            let mut p = input::DispatchMouseEventParams::new(kind, x, y);
            p.button = Some(button.clone());
            p.buttons = Some(buttons);
            p.click_count = Some(1);
            p.modifiers = Some(modifiers);
            self.send(&p).await?;
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        Ok(())
    }

    /// `localStorage` of the page's origin (`DOMStorage.getDOMStorageItems`).
    pub async fn local_storage(&self) -> Result<HashMap<String, String>> {
        let id = self.local_storage_id().await?;
        let r = self.send(&dom_storage::GetDOMStorageItemsParams::new(id)).await?;
        Ok(r.entries
            .into_iter()
            .filter_map(|e| {
                let mut it = e.into_iter();
                Some((it.next()?, it.next().unwrap_or_default()))
            })
            .collect())
    }

    /// Sets `localStorage` items of the page's origin (`DOMStorage.setDOMStorageItem`).
    pub async fn set_local_storage<I, K, V>(&self, items: I) -> Result<()>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let id = self.local_storage_id().await?;
        for (k, v) in items {
            self.send(&dom_storage::SetDOMStorageItemParams::new(
                id.clone(),
                k.as_ref(),
                v.as_ref(),
            ))
            .await?;
        }
        Ok(())
    }

    async fn local_storage_id(&self) -> Result<dom_storage::StorageId> {
        let origin: String = self.evaluate_as("location.origin").await?;
        if origin == "null" || origin.is_empty() {
            return Err(Error::NotFound(
                "the page has no origin with local storage".into(),
            ));
        }
        Ok(dom_storage::StorageId {
            security_origin: Some(origin),
            storage_key: None,
            is_local_storage: true,
        })
    }

    /// Nodes matching an XPath expression (elements, text or attribute nodes), retried until some
    /// appear or `timeout` passes; an empty list when none appear. An invalid expression is an
    /// error (nodriver returns an empty list).
    pub async fn xpath(&self, expression: &str, timeout: Duration) -> Result<Vec<Element>> {
        let expr = format!(
            "((x) => {{ const r = document.evaluate(x, document, null, \
             XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null); const out = []; \
             for (let i = 0; i < r.snapshotLength; i++) out.push(r.snapshotItem(i)); \
             return out; }})({})",
            js_str(expression)
        );
        let start = Instant::now();
        loop {
            let found = self.evaluate_elements(&expr).await?;
            if !found.is_empty() || start.elapsed() > timeout {
                return Ok(found);
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Frame tree of the page (`Page.getFrameTree`).
    pub async fn frame_tree(&self) -> Result<page::FrameTree> {
        Ok(self.send(&page::GetFrameTreeParams::new()).await?.frame_tree)
    }

    /// Frames and the resources each loaded (`Page.getResourceTree`).
    pub async fn frame_resource_tree(&self) -> Result<page::FrameResourceTree> {
        self.ensure_page_enabled().await?;
        Ok(self.send(&page::GetResourceTreeParams::new()).await?.frame_tree)
    }

    /// URLs of every resource loaded by the page and its frames (nodriver's
    /// `get_frame_resource_urls`).
    pub async fn frame_resource_urls(&self) -> Result<Vec<String>> {
        fn walk(t: &page::FrameResourceTree, out: &mut Vec<String>) {
            out.extend(t.resources.iter().map(|r| r.url.clone()));
            for c in t.child_frames.iter().flatten() {
                walk(c, out);
            }
        }
        let mut out = Vec::new();
        walk(&self.frame_resource_tree().await?, &mut out);
        Ok(out)
    }

    /// Allows downloads and saves them to `path` (created if missing), browser-wide.
    pub async fn set_download_path(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = std::path::absolute(path.as_ref())?;
        std::fs::create_dir_all(&path)?;
        let mut p =
            cdp_browser::SetDownloadBehaviorParams::new(cdp_browser::SetDownloadBehaviorBehavior::Allow);
        p.download_path = Some(path.display().to_string());
        self.inner.conn.send(&p, None).await?;
        Ok(())
    }

    /// Downloads `url` with the page's session (fetch, then a click on an `<a download>`), saved
    /// as `filename` (default: last part of the URL) in the folder of [`Tab::set_download_path`].
    pub async fn download_file(&self, url: &str, filename: Option<&str>) -> Result<()> {
        let name = match filename {
            Some(n) => n.to_string(),
            None => url
                .split(['?', '#'])
                .next()
                .unwrap_or("")
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_string(),
        };
        self.evaluate(&format!(
            "(async (u, n) => {{ const r = await fetch(u); const b = await r.blob(); \
             const h = URL.createObjectURL(b); const a = document.createElement('a'); \
             a.href = h; a.download = n; document.body.appendChild(a); a.click(); \
             setTimeout(() => {{ a.remove(); URL.revokeObjectURL(h); }}, 500); }})({}, {})",
            js_str(url),
            js_str(&name)
        ))
        .await?;
        Ok(())
    }

    /// `a`, `link`, `img`, `script` and `meta` elements (nodriver's `get_all_linked_sources`).
    pub async fn linked_sources(&self) -> Result<Vec<Element>> {
        self.query_all("a,link,img,script,meta").await
    }

    /// `src`/`href` of `a`, `link`, `img`, `script` and `meta` elements (nodriver's
    /// `get_all_urls`). With `absolute`, values are resolved against the page and only http(s)
    /// and ws(s) URLs are kept; fragment links are skipped.
    pub async fn get_all_urls(&self, absolute: bool) -> Result<Vec<String>> {
        self.evaluate_as(&format!(
            "((abs) => {{ const out = []; \
             for (const el of document.querySelectorAll('a,link,img,script,meta')) {{ \
               for (const k of ['src', 'href']) {{ const v = el.getAttribute(k); \
                 if (!v) continue; \
                 if (!abs) {{ out.push(v); continue; }} \
                 if (v.startsWith('#')) continue; \
                 try {{ const u = new URL(v, document.baseURI); \
                   if (/^(https?|wss?):$/.test(u.protocol)) out.push(u.href); }} catch (e) {{}} }} }} \
             return out; }})({absolute})"
        ))
        .await
    }

    /// Types `thisisunsafe` on the page, which makes Chrome/Edge proceed past an invalid
    /// certificate warning.
    pub async fn bypass_insecure_connection_warning(&self) -> Result<()> {
        self.evaluate("document.body && document.body.focus()").await?;
        self.type_text("thisisunsafe").await
    }

    /// DevTools front-end URL for this tab, to open in another browser (nodriver's
    /// `inspector_url`).
    pub fn inspector_url(&self) -> String {
        let ws = self.inner.conn.websocket_url();
        let hostport = ws
            .split_once("://")
            .map(|(_, r)| r.split('/').next().unwrap_or(""))
            .unwrap_or("");
        format!(
            "http://{hostport}/devtools/inspector.html?ws={hostport}/devtools/page/{}",
            self.inner.target_id
        )
    }

    /// Frees element handles created by this crate in this tab.
    pub async fn release_handles(&self) -> Result<()> {
        self.send(&runtime::ReleaseObjectGroupParams::new(OBJECT_GROUP))
            .await?;
        Ok(())
    }
}

/// Image format of screenshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageFormat {
    #[default]
    Png,
    Jpeg,
}

impl ImageFormat {
    pub(crate) fn cdp(self) -> page::CaptureScreenshotFormat {
        match self {
            ImageFormat::Png => page::CaptureScreenshotFormat::Png,
            ImageFormat::Jpeg => page::CaptureScreenshotFormat::Jpeg,
        }
    }

    /// JPEG for `.jpg`/`.jpeg`, PNG otherwise.
    pub fn from_path(path: &Path) -> Self {
        match path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .as_deref()
        {
            Some("jpg" | "jpeg") => ImageFormat::Jpeg,
            _ => ImageFormat::Png,
        }
    }
}

fn normal_bounds() -> cdp_browser::Bounds {
    cdp_browser::Bounds {
        left: None,
        top: None,
        width: None,
        height: None,
        window_state: Some(cdp_browser::WindowState::Normal),
    }
}

pub(crate) fn write_file(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, data)?;
    Ok(())
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

const SHIFT: i64 = 8;

struct Modifier {
    key: &'static str,
    code: &'static str,
    vk: i64,
    /// `Input.dispatchKeyEvent` modifier bit: Alt=1, Ctrl=2, Meta=4, Shift=8.
    bit: i64,
}

/// Splits `Control+Shift+k` into the modifiers and the key. A trailing `+` is the key itself.
fn split_modifiers(combo: &str) -> (Vec<Modifier>, &str) {
    let mut mods = Vec::new();
    let mut rest = combo;
    while let Some(i) = rest.find('+') {
        if i == 0 || i + 1 == rest.len() {
            break;
        }
        let m = match rest[..i].to_ascii_lowercase().as_str() {
            "control" | "ctrl" => Modifier {
                key: "Control",
                code: "ControlLeft",
                vk: 17,
                bit: 2,
            },
            "shift" => Modifier {
                key: "Shift",
                code: "ShiftLeft",
                vk: 16,
                bit: SHIFT,
            },
            "alt" => Modifier {
                key: "Alt",
                code: "AltLeft",
                vk: 18,
                bit: 1,
            },
            "meta" | "cmd" | "win" => Modifier {
                key: "Meta",
                code: "MetaLeft",
                vk: 91,
                bit: 4,
            },
            _ => break,
        };
        mods.push(m);
        rest = &rest[i + 1..];
    }
    (mods, rest)
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
        "Insert" => named("Insert", "Insert", 45, None),
        "ArrowUp" => named("ArrowUp", "ArrowUp", 38, None),
        "ArrowDown" => named("ArrowDown", "ArrowDown", 40, None),
        "ArrowLeft" => named("ArrowLeft", "ArrowLeft", 37, None),
        "ArrowRight" => named("ArrowRight", "ArrowRight", 39, None),
        "Home" => named("Home", "Home", 36, None),
        "End" => named("End", "End", 35, None),
        "PageUp" => named("PageUp", "PageUp", 33, None),
        "PageDown" => named("PageDown", "PageDown", 34, None),
        "Space" | " " => named(" ", "Space", 32, Some(" ")),
        f if f.len() >= 2
            && f.starts_with('F')
            && f[1..].parse::<u8>().is_ok_and(|n| (1..=12).contains(&n)) =>
        {
            let n: i64 = f[1..].parse().unwrap();
            named(f, f, 111 + n, None)
        }
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
