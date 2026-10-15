//! Chrome DevTools Protocol types, commands and events.
//!
//! Everything under `generated/` is produced by `cdp-gen` from `protocol/*.json`
//! (<https://github.com/ChromeDevTools/devtools-protocol>). To regenerate:
//! `cargo run -p cdp-gen && cargo fmt -p nodriver-cdp`.
#![allow(
    rustdoc::bare_urls,
    rustdoc::broken_intra_doc_links,
    rustdoc::invalid_html_tags
)]
#![allow(rustdoc::invalid_rust_codeblocks)]

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

mod generated;
pub use generated::*;

/// A CDP command: the struct serializes into `params` and the reply deserializes into `Returns`.
pub trait Command: Serialize {
    /// Method name, e.g. `"Page.navigate"`.
    const METHOD: &'static str;
    /// Type of the reply's `result`.
    type Returns: DeserializeOwned;
}

/// A CDP event: `params` deserializes into the implementing type.
pub trait Event: DeserializeOwned {
    /// Event name, e.g. `"Page.loadEventFired"`.
    const METHOD: &'static str;
}

/// Empty reply (`{}`) of commands that return nothing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Empty {}
