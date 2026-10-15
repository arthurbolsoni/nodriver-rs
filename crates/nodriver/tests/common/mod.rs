#![allow(dead_code)]

use std::path::PathBuf;

use nodriver::{Browser, Config};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serves `tests/fixtures` on 127.0.0.1 and `/api/data` as JSON. Returns the base URL. Also
/// answers proxy requests (absolute URI) with the same files.
pub async fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                break;
            };
            let root = root.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let mut path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                // Absolute URI: the browser uses this server as its HTTP proxy.
                if let Some(rest) = path.strip_prefix("http://") {
                    path = rest.find('/').map_or("/".into(), |i| rest[i..].to_string());
                }
                let (status, ctype, body): (&str, &str, Vec<u8>) = if path == "/api/data" {
                    (
                        "200 OK",
                        "application/json",
                        br#"{"ok":true,"items":[1,2,3]}"#.to_vec(),
                    )
                } else {
                    let file = root.join(path.trim_start_matches('/'));
                    match std::fs::read(&file) {
                        Ok(b) => ("200 OK", "text/html; charset=utf-8", b),
                        Err(_) => ("404 Not Found", "text/plain", b"not found".to_vec()),
                    }
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes()).await;
                let _ = s.write_all(&body).await;
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// Headless browser with a temporary profile. `NODRIVER_HEADFUL=1` shows the window.
pub fn config() -> Config {
    let headful = std::env::var("NODRIVER_HEADFUL").is_ok_and(|v| v == "1");
    Config::new().headless(!headful).arg("--disable-sync")
}

pub async fn browser() -> Browser {
    Browser::launch(config()).await.expect("launch browser")
}
