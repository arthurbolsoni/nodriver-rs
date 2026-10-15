//! A handle to a DOM element (a `Runtime.RemoteObject`). Mirrors nodriver's `core/element.py`.

use std::path::Path;

use nodriver_cdp::{dom, runtime};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::tab::{OBJECT_GROUP, Tab, array_elements, exception_text, js_str};

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
