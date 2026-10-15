use std::path::PathBuf;

/// Errors returned by nodriver.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("websocket: {0}")]
    WebSocket(#[from] Box<tokio_tungstenite::tungstenite::Error>),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    /// The browser answered a command with an error.
    #[error("{method}: {message} (code {code})")]
    Protocol {
        method: String,
        code: i64,
        message: String,
        data: Option<String>,
    },

    /// `Page.navigate` reported an error (DNS, connection refused, ...).
    #[error("navigation failed: {0}")]
    Navigation(String),

    /// No answer or condition within the time limit.
    #[error("timed out: {0}")]
    Timeout(String),

    /// The websocket to the browser is closed.
    #[error("connection to the browser is closed")]
    Disconnected,

    /// A JavaScript exception thrown by an evaluated expression.
    #[error("javascript exception: {0}")]
    Js(String),

    /// Element, tab or target not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// No Chrome/Edge executable found; set `Config::executable`.
    #[error("no Chrome or Edge executable found; set Config::executable")]
    BrowserNotFound,

    /// The browser process could not be started or did not expose its debug port.
    #[error("failed to launch the browser: {0}")]
    Launch(String),

    /// Another browser process already uses this profile; Chrome/Edge hand the launch to it.
    #[error("profile {0} is already in use by another browser process")]
    ProfileInUse(PathBuf),
}

impl From<tokio_tungstenite::tungstenite::Error> for Error {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
        Error::WebSocket(Box::new(e))
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
