//! Chrome/Edge automation over the DevTools Protocol, without chromedriver.
//! A Rust port of [nodriver](https://github.com/ultrafunkamsterdam/nodriver).
//!
//! ```no_run
//! # async fn demo() -> nodriver::Result<()> {
//! let browser = nodriver::Browser::launch(nodriver::Config::new()).await?;
//! let tab = browser.get("https://example.com", false).await?;
//! let title: String = tab.evaluate_as("document.title").await?;
//! tab.find("More information", true).await?.mouse_click().await?;
//! browser.close().await?;
//! # Ok(()) }
//! ```
//!
//! Domains are enabled only when needed (`Page.enable` before the first navigation);
//! `Runtime.enable` is never sent.

mod browser;
mod config;
mod connection;
mod element;
mod error;
mod tab;
mod util;

pub use browser::Browser;
pub use config::{Config, DEFAULT_ARGS, find_browser};
pub use connection::{
    Connection, DEFAULT_TIMEOUT, EventStream, RECONNECT_ATTEMPTS, RECONNECTED_EVENT, RawEvent, RawEventStream,
};
pub use element::{Element, Rect};
pub use error::{Error, Result};
pub use nodriver_cdp as cdp;
pub use tab::{DEFAULT_WAIT, OBJECT_GROUP, Tab};
