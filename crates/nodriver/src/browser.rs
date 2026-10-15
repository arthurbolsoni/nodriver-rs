//! Launching or attaching to a browser and managing its tabs. Mirrors nodriver's `core/browser.py`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use nodriver_cdp::{browser as cdp_browser, target};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};

use crate::config::{Config, find_browser};
use crate::connection::Connection;
use crate::cookies::CookieJar;
use crate::error::{Error, Result};
use crate::tab::Tab;

/// `Browser.PermissionType` values tried by [`Browser::grant_all_permissions`].
const PERMISSIONS: &[&str] = &[
    "ar",
    "audioCapture",
    "automaticFullscreen",
    "backgroundFetch",
    "backgroundSync",
    "cameraPanTiltZoom",
    "clipboardReadWrite",
    "clipboardSanitizedWrite",
    "displayCapture",
    "durableStorage",
    "geolocation",
    "handTracking",
    "idleDetection",
    "keyboardLock",
    "localFonts",
    "localNetwork",
    "localNetworkAccess",
    "loopbackNetwork",
    "midi",
    "midiSysex",
    "nfc",
    "notifications",
    "paymentHandler",
    "periodicBackgroundSync",
    "pointerLock",
    "protectedMediaIdentifier",
    "sensors",
    "smartCard",
    "speakerSelection",
    "storageAccess",
    "topLevelStorageAccess",
    "videoCapture",
    "vr",
    "wakeLockScreen",
    "wakeLockSystem",
    "webAppInstallation",
    "webPrinting",
    "windowManagement",
];

/// How long to wait for a launched browser to expose its DevTools endpoint.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Custom process starter for [`Browser::launch_with`].
type Spawn = Box<dyn FnOnce(&Path, &[String]) -> std::io::Result<()> + Send>;

/// How many times a temporary profile is deleted before giving up, and the pause between tries.
/// The browser's child processes release their files a moment after it exits on Windows.
const REMOVE_TRIES: u32 = 40;
const REMOVE_PAUSE: Duration = Duration::from_millis(250);

/// A running browser, launched by this process or attached over its debug port.
///
/// `Browser` is not `Clone`; share it with `Arc<Browser>`. A browser started by
/// [`Browser::launch`] or [`Browser::launch_with`] that is dropped without [`Browser::close`]
/// (including during a panic) is stopped on drop: its process is killed and the temporary
/// profile, if the crate created one, is deleted. The drop blocks the current thread while it
/// waits for the process to exit and for the profile files to be released. A profile passed in
/// [`Config::user_data_dir`] is never deleted, and a browser attached with [`Browser::connect`]
/// is left running.
pub struct Browser {
    conn: Connection,
    ws_url: String,
    host: String,
    port: u16,
    /// What `launch` started; `None` for `connect` and after `close`.
    launched: Mutex<Option<Launched>>,
    /// Pid of the browser process, from `SystemInfo.getProcessInfo`.
    browser_pid: Option<u32>,
    user_data_dir: Option<PathBuf>,
    tabs: Mutex<HashMap<String, Tab>>,
}

/// Process and temporary profile started by `launch`. Dropping it kills the process and deletes
/// the temporary profile; `close` empties it first so the drop has nothing left to do.
struct Launched {
    /// Process started by `launch` (not by `launch_with`). Edge on Windows relaunches itself,
    /// so this process may exit right away while the browser keeps running as its child.
    launcher: Option<Child>,
    /// Pid of the browser process, once known.
    pid: Option<u32>,
    temp_profile: Option<tempfile::TempDir>,
}

impl Drop for Launched {
    fn drop(&mut self) {
        if let Some(mut child) = self.launcher.take() {
            let _ = child.start_kill();
        }
        if let Some(pid) = self.pid.take() {
            kill_process(pid);
            let start = Instant::now();
            while process_alive(pid) && start.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        if let Some(t) = self.temp_profile.take() {
            let path = t.path().to_path_buf();
            for _ in 0..REMOVE_TRIES {
                if std::fs::remove_dir_all(&path).is_ok() || !path.exists() {
                    break;
                }
                std::thread::sleep(REMOVE_PAUSE);
            }
            if path.exists() {
                tracing::warn!("temporary profile left behind: {}", path.display());
            }
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let launched = self
            .launched
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if launched.is_some() {
            // Tabs may still hold the connection; it must not reconnect to the killed browser.
            self.conn.set_auto_reconnect(false);
        }
        drop(launched);
    }
}

impl std::fmt::Debug for Browser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Browser")
            .field("ws_url", &self.ws_url)
            .field("pid", &self.browser_pid)
            .finish()
    }
}

impl Browser {
    /// Launches Chrome/Edge with `config` and connects to it.
    ///
    /// Fails with [`Error::ProfileInUse`] when another browser process holds the profile
    /// (Chrome/Edge would hand the launch over to it).
    pub async fn launch(config: Config) -> Result<Self> {
        Self::launch_inner(config, None).await
    }

    /// Like [`Browser::launch`], but the process is started by `spawn(executable, args)`, for
    /// example on another Windows desktop or inside a job object. The browser is then found
    /// through its debug port as usual, and `close` stops it through CDP and its pid.
    pub async fn launch_with<F>(config: Config, spawn: F) -> Result<Self>
    where
        F: FnOnce(&Path, &[String]) -> std::io::Result<()> + Send + 'static,
    {
        Self::launch_inner(config, Some(Box::new(spawn))).await
    }

    async fn launch_inner(config: Config, spawn: Option<Spawn>) -> Result<Self> {
        let exe = config
            .executable
            .clone()
            .or_else(find_browser)
            .ok_or(Error::BrowserNotFound)?;
        if !exe.is_file() {
            return Err(Error::Launch(format!("executable not found: {}", exe.display())));
        }
        // Holds what is started from here on; an early return or a cancelled launch drops it,
        // which stops the process and deletes the temporary profile.
        let mut launched = Launched {
            launcher: None,
            pid: None,
            temp_profile: None,
        };
        let user_data_dir = match &config.user_data_dir {
            Some(d) => {
                std::fs::create_dir_all(d)?;
                d.clone()
            }
            None => {
                let t = tempfile::Builder::new().prefix("uc_").tempdir()?;
                let path = t.path().to_path_buf();
                launched.temp_profile = Some(t);
                path
            }
        };
        if profile_locked(&user_data_dir) {
            return Err(Error::ProfileInUse(user_data_dir));
        }
        let port = match config.port {
            Some(p) => p,
            None => free_port(&config.host)?,
        };
        let args = config.build_args(&user_data_dir, port);
        tracing::debug!("launching {} {}", exe.display(), args.join(" "));
        launched.launcher = match spawn {
            Some(spawn) => {
                spawn(&exe, &args).map_err(|e| Error::Launch(format!("{}: {e}", exe.display())))?;
                None
            }
            None => Some(
                Command::new(&exe)
                    .args(&args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|e| Error::Launch(format!("{}: {e}", exe.display())))?,
            ),
        };

        let ws_url = wait_debugger(&config.host, port, launched.launcher.as_mut()).await?;
        let conn = Connection::connect(&ws_url).await?;
        let browser_pid = browser_pid(&conn).await;
        launched.pid = browser_pid;
        Ok(Self {
            conn,
            ws_url,
            host: config.host.clone(),
            port,
            launched: Mutex::new(Some(launched)),
            browser_pid,
            user_data_dir: Some(user_data_dir),
            tabs: Mutex::new(HashMap::new()),
        })
    }

    /// Attaches to a browser already running with `--remote-debugging-port`.
    pub async fn connect(host: &str, port: u16) -> Result<Self> {
        let info = http_json(host, port, "/json/version").await?;
        let ws_url = info
            .get("webSocketDebuggerUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Launch("no webSocketDebuggerUrl in /json/version".into()))?
            .to_string();
        let conn = Connection::connect(&ws_url).await?;
        let browser_pid = browser_pid(&conn).await;
        Ok(Self {
            conn,
            ws_url,
            host: host.to_string(),
            port,
            launched: Mutex::new(None),
            browser_pid,
            user_data_dir: None,
            tabs: Mutex::new(HashMap::new()),
        })
    }

    /// The browser-level websocket connection.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn websocket_url(&self) -> &str {
        &self.ws_url
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Profile directory, when this process launched the browser.
    pub fn user_data_dir(&self) -> Option<&Path> {
        self.user_data_dir.as_deref()
    }

    /// Pid of the browser process.
    pub fn pid(&self) -> Option<u32> {
        self.browser_pid
    }

    pub async fn version(&self) -> Result<cdp_browser::GetVersionReturns> {
        self.conn.send(&cdp_browser::GetVersionParams::new(), None).await
    }

    /// Every target (pages, iframes out of process, workers, ...).
    pub async fn targets(&self) -> Result<Vec<target::TargetInfo>> {
        Ok(self
            .conn
            .send(&target::GetTargetsParams::new(), None)
            .await?
            .target_infos)
    }

    /// Open tabs (targets of type `page`), in the order the browser lists them.
    pub async fn tabs(&self) -> Result<Vec<Tab>> {
        let all = self.targets().await?;
        // Cached tabs of other target types (iframes, workers) stay while their target lives.
        let alive: Vec<String> = all.iter().map(|t| t.target_id.clone()).collect();
        let pages: Vec<target::TargetInfo> = all.into_iter().filter(|t| t.r#type == "page").collect();
        self.tabs.lock().unwrap().retain(|id, _| {
            let keep = alive.contains(id);
            if !keep {
                self.conn.forget_target(id);
            }
            keep
        });
        let mut out = Vec::with_capacity(pages.len());
        for t in pages {
            out.push(self.tab(&t.target_id).await?);
        }
        Ok(out)
    }

    /// Tab for a target id, attaching a session on first use.
    pub async fn tab(&self, target_id: &str) -> Result<Tab> {
        if let Some(t) = self.tabs.lock().unwrap().get(target_id) {
            return Ok(t.clone());
        }
        self.conn.attach(target_id).await?;
        let tab = Tab::new(self.conn.clone(), target_id.to_string());
        self.tabs
            .lock()
            .unwrap()
            .insert(target_id.to_string(), tab.clone());
        Ok(tab)
    }

    /// First open tab; opens one if there is none.
    pub async fn main_tab(&self) -> Result<Tab> {
        match self.tabs().await?.into_iter().next() {
            Some(t) => Ok(t),
            None => self.new_tab("about:blank").await,
        }
    }

    /// Opens a new tab and navigates it to `url` (waits for the load event).
    pub async fn new_tab(&self, url: &str) -> Result<Tab> {
        let id = self
            .conn
            .send(&target::CreateTargetParams::new("about:blank"), None)
            .await?
            .target_id;
        let tab = self.tab(&id).await?;
        if url != "about:blank" {
            tab.navigate(url).await?;
        }
        Ok(tab)
    }

    /// Navigates the first tab, or a new tab when `new_tab` is true. Same as nodriver's `get`.
    pub async fn get(&self, url: &str, new_tab: bool) -> Result<Tab> {
        if new_tab {
            return self.new_tab(url).await;
        }
        let tab = self.main_tab().await?;
        tab.navigate(url).await?;
        Ok(tab)
    }

    /// Cookies of the default context (nodriver's `browser.cookies`).
    pub fn cookies(&self) -> CookieJar<'_> {
        CookieJar::new(&self.conn)
    }

    /// Grants every permission type to every origin (geolocation, notifications, clipboard,
    /// camera, ...). Types the browser refuses are skipped; returns the ones granted.
    pub async fn grant_all_permissions(&self) -> Result<Vec<String>> {
        let all: Vec<&str> = PERMISSIONS.to_vec();
        if self
            .conn
            .call("Browser.grantPermissions", json!({ "permissions": all }), None)
            .await
            .is_ok()
        {
            return Ok(all.iter().map(|s| s.to_string()).collect());
        }
        // A call grants its list and rejects every other type, so the accepted types are found
        // one by one and granted together at the end.
        let mut granted = Vec::new();
        for p in all {
            if self
                .conn
                .call("Browser.grantPermissions", json!({ "permissions": [p] }), None)
                .await
                .is_ok()
            {
                granted.push(p.to_string());
            }
        }
        if granted.is_empty() {
            return Err(Error::NotFound("the browser granted no permission".into()));
        }
        self.conn
            .call(
                "Browser.grantPermissions",
                json!({ "permissions": granted }),
                None,
            )
            .await?;
        Ok(granted)
    }

    /// Opens `url` in a tab of a new browser context (separate cookies, storage and cache), with
    /// its own proxy when `proxy_server` is given (`http://host:port`, `socks5://host:port`;
    /// proxies with credentials are not supported). The context is disposed when the tab closes
    /// or this connection ends. Waits for the load event, as [`Browser::new_tab`].
    pub async fn create_context(
        &self,
        url: &str,
        proxy_server: Option<&str>,
        proxy_bypass_list: Option<&str>,
    ) -> Result<Tab> {
        let mut p = target::CreateBrowserContextParams::new();
        p.dispose_on_detach = Some(true);
        p.proxy_server = proxy_server.map(str::to_string);
        p.proxy_bypass_list = proxy_bypass_list.map(str::to_string);
        let ctx = self.conn.send(&p, None).await?.browser_context_id;
        let mut t = target::CreateTargetParams::new("about:blank");
        t.browser_context_id = Some(ctx);
        let id = self.conn.send(&t, None).await?.target_id;
        let tab = self.tab(&id).await?;
        if url != "about:blank" {
            tab.navigate(url).await?;
        }
        Ok(tab)
    }

    /// Turns off automatic reconnection, closes the browser (`Browser.close`), waits for its
    /// process to exit (killing it after 10 s) and deletes the temporary profile, if any.
    /// Without `close`, dropping a launched `Browser` kills the process instead (see [`Browser`]).
    pub async fn close(&self) -> Result<()> {
        self.conn.set_auto_reconnect(false);
        let _ = self
            .conn
            .call_with_timeout(
                "Browser.close",
                serde_json::json!({}),
                None,
                Duration::from_secs(5),
            )
            .await;
        // Taken out of `self` but kept alive until the end: if this future is dropped midway,
        // `Launched`'s drop finishes the job.
        let mut launched = self.launched.lock().unwrap().take();
        if let Some(child) = launched.as_mut().and_then(|l| l.launcher.as_mut())
            && tokio::time::timeout(Duration::from_secs(10), child.wait())
                .await
                .is_err()
        {
            let _ = child.kill().await;
        }
        if let Some(pid) = self.browser_pid {
            let start = Instant::now();
            while process_alive(pid) {
                if start.elapsed() > Duration::from_secs(10) {
                    kill_process(pid);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let Some(l) = launched.as_mut() else {
            return Ok(());
        };
        l.launcher = None;
        l.pid = None;
        if let Some(path) = l.temp_profile.as_ref().map(|t| t.path().to_path_buf()) {
            // Child processes can keep files open for a moment after the browser exits.
            for _ in 0..REMOVE_TRIES {
                if std::fs::remove_dir_all(&path).is_ok() || !path.exists() {
                    break;
                }
                tokio::time::sleep(REMOVE_PAUSE).await;
            }
            if path.exists() {
                tracing::warn!("temporary profile left behind: {}", path.display());
            }
            l.temp_profile = None;
        }
        Ok(())
    }
}

async fn browser_pid(conn: &Connection) -> Option<u32> {
    let info = conn
        .call("SystemInfo.getProcessInfo", serde_json::json!({}), None)
        .await
        .ok()?;
    info.get("processInfo")?
        .as_array()?
        .iter()
        .find(|p| p.get("type").and_then(Value::as_str) == Some("browser"))?
        .get("id")?
        .as_u64()
        .map(|id| id as u32)
}

/// Waits for `/json/version` on the debug port and returns the browser websocket url.
async fn wait_debugger(host: &str, port: u16, mut child: Option<&mut Child>) -> Result<String> {
    let start = Instant::now();
    loop {
        let probe = tokio::time::timeout(Duration::from_secs(1), http_json(host, port, "/json/version"));
        if let Ok(Ok(info)) = probe.await
            && let Some(ws) = info.get("webSocketDebuggerUrl").and_then(Value::as_str)
        {
            return Ok(ws.to_string());
        }
        // Exit code 0 is normal: Edge relaunches itself and the first process exits.
        if let Some(child) = child.as_mut()
            && let Some(status) = child.try_wait()?
            && !status.success()
        {
            return Err(Error::Launch(format!("browser exited with {status}")));
        }
        if start.elapsed() > LAUNCH_TIMEOUT {
            if let Some(child) = child.as_mut() {
                let _ = child.kill().await;
            }
            return Err(Error::Launch(format!("no DevTools endpoint on {host}:{port}")));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// A free TCP port on `host`, as nodriver's `util.free_port`. The port is released before the
/// browser binds it.
///
/// A fixed port also matters for detection: with `--remote-debugging-port=0`, Edge 155 reports
/// `navigator.webdriver === true`; with a fixed port it reports `false`.
fn free_port(host: &str) -> Result<u16> {
    let l = std::net::TcpListener::bind((host, 0))?;
    Ok(l.local_addr()?.port())
}

/// Whether a running browser holds the profile.
///
/// Windows: `lockfile` is open without delete sharing while the browser runs, so deleting it
/// fails. Linux/macOS: `SingletonLock` is a symlink to `<host>-<pid>`.
fn profile_locked(user_data_dir: &Path) -> bool {
    #[cfg(windows)]
    {
        let lock = user_data_dir.join("lockfile");
        lock.exists() && std::fs::remove_file(&lock).is_err()
    }
    #[cfg(not(windows))]
    {
        let Ok(target) = std::fs::read_link(user_data_dir.join("SingletonLock")) else {
            return false;
        };
        let target = target.to_string_lossy().to_string();
        match target
            .rsplit_once('-')
            .and_then(|(_, pid)| pid.parse::<u32>().ok())
        {
            Some(pid) => process_alive(pid),
            None => true,
        }
    }
}

fn process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")))
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

fn kill_process(pid: u32) {
    #[cfg(windows)]
    let _ = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .output();
    #[cfg(not(windows))]
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .output();
}

/// GET on the DevTools HTTP endpoint (`/json/version`, `/json/list`). The endpoint keeps the
/// connection open, so the body is read by `Content-Length`.
pub(crate) async fn http_json(host: &str, port: u16, path: &str) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(10), http_json_inner(host, port, path))
        .await
        .map_err(|_| Error::Timeout(format!("GET http://{host}:{port}{path}")))?
}

async fn http_json_inner(host: &str, port: u16, path: &str) -> Result<Value> {
    let mut s = TcpStream::connect((host, port)).await?;
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).await?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let invalid = || Error::Launch(format!("invalid HTTP reply from {host}:{port}{path}"));
    let (head_len, body_len) = loop {
        let n = s.read(&mut chunk).await?;
        if n == 0 {
            return Err(invalid());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]).to_ascii_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .ok_or_else(invalid)?;
            break (pos + 4, len);
        }
    };
    while buf.len() < head_len + body_len {
        let n = s.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let end = (head_len + body_len).min(buf.len());
    Ok(serde_json::from_slice(&buf[head_len..end])?)
}
