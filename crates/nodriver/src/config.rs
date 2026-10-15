//! Browser launch configuration. Mirrors nodriver's `core/config.py`.

use std::path::{Path, PathBuf};

/// Flags passed to every launch, same list as nodriver's `Config._default_browser_args`.
pub const DEFAULT_ARGS: &[&str] = &[
    "--remote-allow-origins=*",
    "--no-first-run",
    "--no-service-autorun",
    "--no-default-browser-check",
    "--homepage=about:blank",
    "--no-pings",
    "--password-store=basic",
    "--disable-infobars",
    "--disable-breakpad",
    "--disable-dev-shm-usage",
    "--disable-session-crashed-bubble",
    "--disable-search-engine-choice-screen",
];

/// How to launch the browser.
#[derive(Debug, Clone)]
pub struct Config {
    /// Profile directory. `None` creates a temporary profile that is deleted by `Browser::close`
    /// or when the `Browser` is dropped. A directory given here is never deleted.
    pub user_data_dir: Option<PathBuf>,
    /// `--headless=new`.
    pub headless: bool,
    /// Browser executable. `None` searches for Chrome, then Edge ([`find_browser`]).
    pub executable: Option<PathBuf>,
    /// Extra command line flags.
    pub args: Vec<String>,
    /// `false` adds `--no-sandbox`. Turned off automatically for root on Linux/macOS.
    pub sandbox: bool,
    /// UI and `Accept-Language` language (`--lang` and `--accept-lang`). Not set by default.
    pub lang: Option<String>,
    /// `--remote-debugging-host`.
    pub host: String,
    /// `--remote-debugging-port`. `None` picks a free port before launching (port 0 is not used:
    /// Edge reports `navigator.webdriver === true` with it).
    pub port: Option<u16>,
    /// Adds `--disable-site-isolation-trials` (nodriver's "expert" mode flag).
    pub expert: bool,
    /// Unpacked extension directories to load.
    pub extensions: Vec<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            user_data_dir: None,
            headless: false,
            executable: None,
            args: Vec::new(),
            sandbox: !is_root(),
            lang: None,
            host: "127.0.0.1".into(),
            port: None,
            expert: false,
            extensions: Vec::new(),
        }
    }
}

impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn user_data_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.user_data_dir = Some(dir.into());
        self
    }

    pub fn headless(mut self, headless: bool) -> Self {
        self.headless = headless;
        self
    }

    pub fn executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.executable = Some(path.into());
        self
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn sandbox(mut self, sandbox: bool) -> Self {
        self.sandbox = sandbox;
        self
    }

    pub fn lang(mut self, lang: impl Into<String>) -> Self {
        self.lang = Some(lang.into());
        self
    }

    pub fn port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    pub fn expert(mut self, expert: bool) -> Self {
        self.expert = expert;
        self
    }

    pub fn extension(mut self, dir: impl Into<PathBuf>) -> Self {
        self.extensions.push(dir.into());
        self
    }

    /// Command line for the browser, in the same order nodriver builds it.
    pub fn build_args(&self, user_data_dir: &Path, port: u16) -> Vec<String> {
        let mut args: Vec<String> = DEFAULT_ARGS.iter().map(|s| s.to_string()).collect();
        args.push(format!("--user-data-dir={}", user_data_dir.display()));
        args.push("--disable-session-crashed-bubble".into());
        let mut disabled = String::from("IsolateOrigins,site-per-process");
        if !self.extensions.is_empty() {
            disabled.push_str(",DisableLoadExtensionCommandLineSwitch");
        }
        args.push(format!("--disable-features={disabled}"));
        if !self.extensions.is_empty() {
            args.push("--enable-unsafe-extension-debugging".into());
            let list: Vec<String> = self.extensions.iter().map(|p| p.display().to_string()).collect();
            args.push(format!("--load-extension={}", list.join(",")));
        }
        if self.expert {
            args.push("--disable-site-isolation-trials".into());
        }
        if let Some(lang) = &self.lang {
            args.push(format!("--lang={lang}"));
            args.push(format!("--accept-lang={lang}"));
        }
        for a in &self.args {
            if !args.contains(a) {
                args.push(a.clone());
            }
        }
        if self.headless {
            args.push("--headless=new".into());
        }
        if !self.sandbox {
            args.push("--no-sandbox".into());
        }
        args.push(format!("--remote-debugging-host={}", self.host));
        args.push(format!("--remote-debugging-port={port}"));
        args
    }
}

/// Finds a Chrome executable (stable, beta, canary; shortest path wins, as in nodriver),
/// falling back to Microsoft Edge.
pub fn find_browser() -> Option<PathBuf> {
    let chrome = candidates(false)
        .into_iter()
        .filter(|p| p.is_file())
        .min_by_key(|p| p.as_os_str().len());
    chrome.or_else(|| candidates(true).into_iter().find(|p| p.is_file()))
}

fn candidates(edge: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if cfg!(windows) {
        let subdirs: &[&str] = if edge {
            &[
                "Microsoft/Edge/Application/msedge.exe",
                "Microsoft/Edge Beta/Application/msedge.exe",
            ]
        } else {
            &[
                "Google/Chrome/Application/chrome.exe",
                "Google/Chrome Beta/Application/chrome.exe",
                "Google/Chrome Canary/Application/chrome.exe",
            ]
        };
        for var in [
            "PROGRAMFILES",
            "PROGRAMFILES(X86)",
            "LOCALAPPDATA",
            "PROGRAMW6432",
        ] {
            if let Some(base) = std::env::var_os(var) {
                for sub in subdirs {
                    out.push(Path::new(&base).join(sub));
                }
            }
        }
    } else {
        let names: &[&str] = if edge {
            &["microsoft-edge", "microsoft-edge-stable"]
        } else {
            &[
                "google-chrome",
                "chromium",
                "chromium-browser",
                "chrome",
                "google-chrome-stable",
            ]
        };
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                for n in names {
                    out.push(dir.join(n));
                }
            }
        }
        if cfg!(target_os = "macos") {
            if edge {
                out.push("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge".into());
            } else {
                out.push("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into());
                out.push("/Applications/Chromium.app/Contents/MacOS/Chromium".into());
            }
        }
    }
    out
}

/// Root on Linux/macOS (the sandbox must be disabled there). Always `false` on Windows,
/// same as nodriver, which only checks this on POSIX.
fn is_root() -> bool {
    #[cfg(unix)]
    {
        std::env::var("USER").map(|u| u == "root").unwrap_or(false)
            || std::env::var("EUID").map(|u| u == "0").unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_order_and_flags() {
        let c = Config::new()
            .lang("pt-BR")
            .arg("--window-size=1400,950")
            .headless(true);
        let a = c.build_args(Path::new("/tmp/p"), 9222);
        assert_eq!(&a[..DEFAULT_ARGS.len()], DEFAULT_ARGS);
        assert!(a.contains(&"--user-data-dir=/tmp/p".to_string()));
        assert!(a.contains(&"--disable-features=IsolateOrigins,site-per-process".to_string()));
        assert!(a.contains(&"--lang=pt-BR".to_string()));
        assert!(a.contains(&"--headless=new".to_string()));
        assert_eq!(a.last().unwrap(), "--remote-debugging-port=9222");
        assert!(!a.iter().any(|x| x.contains("enable-automation")));
    }

    #[test]
    fn extra_arg_not_duplicated() {
        let c = Config::new().arg("--no-first-run");
        let a = c.build_args(Path::new("p"), 0);
        assert_eq!(a.iter().filter(|x| *x == "--no-first-run").count(), 1);
    }
}
