//! A handle to a DOM element (a `Runtime.RemoteObject`). Mirrors nodriver's `core/element.py`.

use std::collections::HashMap;
use std::path::Path;

use nodriver_cdp::{dom, input, page, runtime};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::tab::{ImageFormat, OBJECT_GROUP, Tab, array_elements, exception_text, js_str, write_file};

/// Element handle. Valid while the node exists in the page; a re-render that replaces the node
/// leaves the handle pointing to the detached node.
#[derive(Clone)]
pub struct Element {
    tab: Tab,
    object_id: String,
}

impl std::fmt::Debug for Element {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Element")
            .field("object_id", &self.object_id)
            .finish()
    }
}

/// Position of an element in viewport CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn center(&self) -> (f64, f64) {
        (self.left + self.width / 2.0, self.top + self.height / 2.0)
    }
}

impl Element {
    pub(crate) fn new(tab: Tab, object_id: String) -> Self {
        Self { tab, object_id }
    }

    pub fn tab(&self) -> &Tab {
        &self.tab
    }

    pub fn object_id(&self) -> &str {
        &self.object_id
    }

    /// Calls `function` with the element as its first argument and returns the JSON result,
    /// e.g. `el.apply("(el) => el.value")`. Promises are awaited.
    pub async fn apply(&self, function: &str) -> Result<Value> {
        let r = self.call_function(function, true).await?;
        Ok(r.value.unwrap_or(Value::Null))
    }

    pub async fn apply_as<T: DeserializeOwned>(&self, function: &str) -> Result<T> {
        Ok(serde_json::from_value(self.apply(function).await?)?)
    }

    async fn call_function(&self, function: &str, by_value: bool) -> Result<runtime::RemoteObject> {
        let mut p = runtime::CallFunctionOnParams::new(function);
        p.object_id = Some(self.object_id.clone());
        p.arguments = Some(vec![runtime::CallArgument {
            value: None,
            unserializable_value: None,
            object_id: Some(self.object_id.clone()),
        }]);
        p.await_promise = Some(true);
        p.return_by_value = Some(by_value);
        p.user_gesture = Some(true);
        p.object_group = Some(OBJECT_GROUP.into());
        let r = self.tab.send(&p).await?;
        if let Some(ex) = r.exception_details {
            return Err(Error::Js(exception_text(&ex)));
        }
        Ok(r.result)
    }

    /// First descendant matching a CSS selector.
    pub async fn query(&self, selector: &str) -> Result<Option<Element>> {
        let obj = self
            .call_function(&format!("(el) => el.querySelector({})", js_str(selector)), false)
            .await?;
        Ok(obj.object_id.map(|id| Element::new(self.tab.clone(), id)))
    }

    /// Every descendant matching a CSS selector.
    pub async fn query_all(&self, selector: &str) -> Result<Vec<Element>> {
        let obj = self
            .call_function(
                &format!("(el) => Array.from(el.querySelectorAll({}))", js_str(selector)),
                false,
            )
            .await?;
        match obj.object_id {
            Some(id) => array_elements(&self.tab, &id).await,
            None => Ok(Vec::new()),
        }
    }

    /// `innerText` (falls back to `textContent`).
    pub async fn text(&self) -> Result<String> {
        self.apply_as("(el) => (el.innerText ?? el.textContent ?? '')")
            .await
    }

    pub async fn outer_html(&self) -> Result<String> {
        self.apply_as("(el) => el.outerHTML").await
    }

    pub async fn attribute(&self, name: &str) -> Result<Option<String>> {
        self.apply_as(&format!("(el) => el.getAttribute({})", js_str(name)))
            .await
    }

    /// `value` of an input, textarea or select.
    pub async fn value(&self) -> Result<Option<String>> {
        self.apply_as("(el) => (el.value === undefined ? null : String(el.value))")
            .await
    }

    /// Lowercase tag name.
    pub async fn tag(&self) -> Result<String> {
        self.apply_as("(el) => el.tagName.toLowerCase()").await
    }

    /// Whether the node is still attached to the document.
    pub async fn is_connected(&self) -> Result<bool> {
        self.apply_as("(el) => el.isConnected").await
    }

    pub async fn scroll_into_view(&self) -> Result<()> {
        let mut p = dom::ScrollIntoViewIfNeededParams::new();
        p.object_id = Some(self.object_id.clone());
        self.tab.send(&p).await?;
        Ok(())
    }

    /// Box of the first content quad, in viewport CSS pixels.
    pub async fn rect(&self) -> Result<Rect> {
        let mut p = dom::GetContentQuadsParams::new();
        p.object_id = Some(self.object_id.clone());
        let r = self.tab.send(&p).await?;
        let q = r
            .quads
            .into_iter()
            .find(|q| q.len() >= 8)
            .ok_or_else(|| Error::NotFound("element has no layout box (hidden or detached)".into()))?;
        let xs = [q[0], q[2], q[4], q[6]];
        let ys = [q[1], q[3], q[5], q[7]];
        let (l, r_) = (
            xs.iter().cloned().fold(f64::MAX, f64::min),
            xs.iter().cloned().fold(f64::MIN, f64::max),
        );
        let (t, b) = (
            ys.iter().cloned().fold(f64::MAX, f64::min),
            ys.iter().cloned().fold(f64::MIN, f64::max),
        );
        Ok(Rect {
            left: l,
            top: t,
            width: r_ - l,
            height: b - t,
        })
    }

    /// `el.click()` from JavaScript with a user gesture (nodriver's `click`).
    pub async fn click(&self) -> Result<()> {
        self.apply("(el) => el.click()").await?;
        Ok(())
    }

    /// Real mouse click at the element's center: scroll into view, move, press, release
    /// (nodriver's `mouse_click`).
    pub async fn mouse_click(&self) -> Result<()> {
        self.scroll_into_view().await?;
        let (x, y) = self.rect().await?.center();
        self.tab.mouse_click(x, y).await
    }

    /// Moves the mouse to the element's center (triggers hover).
    pub async fn mouse_move(&self) -> Result<()> {
        self.scroll_into_view().await?;
        let (x, y) = self.rect().await?.center();
        self.tab.mouse_move(x, y).await
    }

    pub async fn focus(&self) -> Result<()> {
        self.apply("(el) => el.focus()").await?;
        Ok(())
    }

    /// Focuses and types `text` as `char` key events (nodriver's `send_keys`).
    pub async fn send_keys(&self, text: &str) -> Result<()> {
        self.focus().await?;
        self.tab.type_text(text).await
    }

    /// Sets `value` to an empty string (nodriver's `clear_input`).
    pub async fn clear_input(&self) -> Result<()> {
        self.apply("(el) => { el.value = '' }").await?;
        Ok(())
    }

    /// Sets the files of an `<input type=file>` (works for hidden inputs and inside iframes).
    pub async fn send_file<P: AsRef<Path>>(&self, paths: &[P]) -> Result<()> {
        let mut files = Vec::with_capacity(paths.len());
        for p in paths {
            let abs = std::path::absolute(p.as_ref())?;
            files.push(abs.display().to_string());
        }
        let mut p = dom::SetFileInputFilesParams::new(files);
        p.object_id = Some(self.object_id.clone());
        self.tab.send(&p).await?;
        Ok(())
    }

    /// Text of every descendant text node, trimmed and joined with spaces; includes `<script>`
    /// and hidden text (nodriver's `text_all`).
    pub async fn text_all(&self) -> Result<String> {
        self.apply_as(
            "(el) => { const w = document.createTreeWalker(el, NodeFilter.SHOW_TEXT); const out = []; \
             if (el.nodeType === 3) out.push(el.nodeValue.trim()); \
             while (w.nextNode()) { const t = w.currentNode.nodeValue.trim(); if (t) out.push(t); } \
             return out.filter(Boolean).join(' '); }",
        )
        .await
    }

    /// Every attribute, by name.
    pub async fn attributes(&self) -> Result<HashMap<String, String>> {
        self.apply_as("(el) => Object.fromEntries(Array.from(el.attributes || [], (a) => [a.name, a.value]))")
            .await
    }

    pub async fn set_attribute(&self, name: &str, value: &str) -> Result<()> {
        self.apply(&format!(
            "(el) => el.setAttribute({}, {})",
            js_str(name),
            js_str(value)
        ))
        .await?;
        Ok(())
    }

    pub async fn remove_attribute(&self, name: &str) -> Result<()> {
        self.apply(&format!("(el) => el.removeAttribute({})", js_str(name)))
            .await?;
        Ok(())
    }

    /// Parent element; `None` for the root.
    pub async fn parent(&self) -> Result<Option<Element>> {
        let obj = self.call_function("(el) => el.parentElement", false).await?;
        Ok(obj.object_id.map(|id| Element::new(self.tab.clone(), id)))
    }

    /// Child elements; for a same-origin `<iframe>`, the children of its document.
    pub async fn children(&self) -> Result<Vec<Element>> {
        let obj = self
            .call_function(
                "(el) => { let d = null; try { d = el.contentDocument; } catch (e) {} \
                 return Array.from((d || el).children || []); }",
                false,
            )
            .await?;
        match obj.object_id {
            Some(id) => array_elements(&self.tab, &id).await,
            None => Ok(Vec::new()),
        }
    }

    /// Removes the node from the document (nodriver's `remove_from_dom`).
    pub async fn remove(&self) -> Result<()> {
        self.apply("(el) => el.remove()").await?;
        Ok(())
    }

    /// Calls a method of the element without arguments, e.g. `play` on a video (nodriver's
    /// `element(js_method)`).
    pub async fn call_method(&self, method: &str) -> Result<Value> {
        self.apply(&format!("(el) => el[{}]()", js_str(method))).await
    }

    /// Sets `value` with the native setter and fires `input` and `change`, so frameworks with
    /// controlled inputs see it. nodriver's `set_value` uses `DOM.setNodeValue`, which only
    /// changes text nodes; that is [`Element::set_text`] here.
    pub async fn set_value(&self, value: &str) -> Result<()> {
        self.apply(&format!(
            "(el) => {{ const p = Object.getPrototypeOf(el); \
             const d = Object.getOwnPropertyDescriptor(p, 'value'); \
             if (d && d.set) d.set.call(el, {v}); else el.value = {v}; \
             el.dispatchEvent(new Event('input', {{bubbles: true}})); \
             el.dispatchEvent(new Event('change', {{bubbles: true}})); }}",
            v = js_str(value)
        ))
        .await?;
        Ok(())
    }

    /// Replaces the text of a text node, or of an element whose only child is a text node
    /// (nodriver's `set_text`).
    pub async fn set_text(&self, value: &str) -> Result<()> {
        self.apply(&format!(
            "(el) => {{ const t = el.nodeType === 3 ? el \
             : (el.childNodes.length === 1 && el.firstChild.nodeType === 3 ? el.firstChild : null); \
             if (!t) throw new Error('set_text needs a text node or an element with one text child'); \
             t.nodeValue = {}; }}",
            js_str(value)
        ))
        .await?;
        Ok(())
    }

    /// Box in document coordinates (viewport box plus the scroll offset), as nodriver's
    /// `get_position(abs=True)`.
    pub async fn document_rect(&self) -> Result<Rect> {
        let r = self.rect().await?;
        let (sx, sy): (f64, f64) = self
            .tab
            .evaluate_as("[window.scrollX, window.scrollY]")
            .await
            .map(|v: Vec<f64>| (v[0], v[1]))?;
        Ok(Rect {
            left: r.left + sx,
            top: r.top + sy,
            ..r
        })
    }

    /// Click at the element's center with any button and modifiers.
    pub async fn mouse_click_with(&self, button: input::MouseButton, modifiers: i64) -> Result<()> {
        self.scroll_into_view().await?;
        let (x, y) = self.rect().await?.center();
        self.tab.mouse_click_with(x, y, button, modifiers).await
    }

    /// Drags from the element's center to viewport point `(x, y)` (nodriver's `mouse_drag`).
    pub async fn mouse_drag_to(&self, x: f64, y: f64, steps: u32) -> Result<()> {
        let from = self.drag_start().await?;
        self.tab.mouse_drag(from, (x, y), steps).await
    }

    /// Drags from the element's center by `(dx, dy)` pixels (nodriver's `mouse_drag` with
    /// `relative=True`).
    pub async fn mouse_drag_by(&self, dx: f64, dy: f64, steps: u32) -> Result<()> {
        let from = self.drag_start().await?;
        self.tab.mouse_drag(from, (from.0 + dx, from.1 + dy), steps).await
    }

    /// Drags from the element's center to the center of `target`.
    pub async fn mouse_drag_to_element(&self, target: &Element, steps: u32) -> Result<()> {
        let from = self.drag_start().await?;
        let to = target.rect().await?.center();
        self.tab.mouse_drag(from, to, steps).await
    }

    async fn drag_start(&self) -> Result<(f64, f64)> {
        self.scroll_into_view().await?;
        Ok(self.rect().await?.center())
    }

    /// Scrolls the element's own content (a list with its own scrollbar) down by `percent` of the
    /// element's height, with a scroll gesture at its center. Scrolling chains to the page once
    /// the element reaches its end.
    pub async fn scroll_down(&self, percent: f64) -> Result<()> {
        let r = self.rect().await?;
        let (x, y) = r.center();
        self.tab
            .scroll_gesture(x, y, 0.0, r.height * percent / 100.0)
            .await
    }

    /// Scrolls the element's own content up; see [`Element::scroll_down`].
    pub async fn scroll_up(&self, percent: f64) -> Result<()> {
        let r = self.rect().await?;
        let (x, y) = r.center();
        self.tab
            .scroll_gesture(x, y, 0.0, -r.height * percent / 100.0)
            .await
    }

    /// Screenshot of the element's box, scrolled into view first; `scale` 1 keeps the CSS size
    /// (nodriver's `save_screenshot` of an element).
    pub async fn screenshot(&self, format: ImageFormat, scale: f64) -> Result<Vec<u8>> {
        self.scroll_into_view().await?;
        let r = self.rect().await?;
        if r.width <= 0.0 || r.height <= 0.0 {
            return Err(Error::NotFound("element has an empty box".into()));
        }
        let m = self.tab.send(&page::GetLayoutMetricsParams::new()).await?;
        let mut p = page::CaptureScreenshotParams::new();
        p.format = Some(format.cdp());
        p.clip = Some(page::Viewport {
            x: r.left + m.css_visual_viewport.page_x,
            y: r.top + m.css_visual_viewport.page_y,
            width: r.width,
            height: r.height,
            scale,
        });
        p.capture_beyond_viewport = Some(true);
        let data = self.tab.send(&p).await?.data;
        crate::util::base64_decode(&data).ok_or_else(|| Error::Js("invalid base64 screenshot".into()))
    }

    /// Writes a screenshot of the element to `path`; JPEG for `.jpg`/`.jpeg`, PNG otherwise.
    pub async fn save_screenshot(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let data = self.screenshot(ImageFormat::from_path(path), 1.0).await?;
        write_file(path, &data)
    }

    /// Selects this `<option>` and fires `change` on its `<select>` (nodriver's `select_option`).
    pub async fn select_option(&self) -> Result<()> {
        self.apply(
            "(el) => { if (el.tagName !== 'OPTION') throw new Error('not an <option>'); \
             el.selected = true; const s = el.closest('select') || el.parentElement; \
             s.dispatchEvent(new Event('input', {bubbles: true})); \
             s.dispatchEvent(new Event('change', {bubbles: true})); }",
        )
        .await?;
        Ok(())
    }
}
