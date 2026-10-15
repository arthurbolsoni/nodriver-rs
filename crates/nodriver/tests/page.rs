mod common;

use std::time::{Duration, Instant};

use nodriver::Error;
use nodriver::cdp::browser::WindowState;

#[tokio::test(flavor = "multi_thread")]
async fn history_back_and_forward() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/basic.html"), false).await.unwrap();
    match tab.back().await {
        Err(Error::NotFound(_)) => {}
        other => panic!("expected NotFound on first entry, got {other:?}"),
    }
    tab.navigate(&format!("{base}/page2.html")).await.unwrap();
    tab.back().await.unwrap();
    assert_eq!(tab.title().await.unwrap(), "Basic");
    tab.forward().await.unwrap();
    assert_eq!(tab.title().await.unwrap(), "Second");
    // Same-document entry.
    tab.navigate(&format!("{base}/page2.html#part")).await.unwrap();
    tab.back().await.unwrap();
    let u = tab.url().await.unwrap();
    assert!(u.ends_with("/page2.html"), "{u}");
    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn content_and_reload_with() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/basic.html"), false).await.unwrap();
    let html = tab.content().await.unwrap();
    assert!(
        html.to_lowercase().starts_with("<!doctype html>"),
        "{}",
        &html[..40]
    );
    assert!(html.contains("<title>Basic</title>"));
    tab.evaluate("window.marker = 1").await.unwrap();
    tab.reload_with(true, None).await.unwrap();
    assert_eq!(
        tab.evaluate("window.marker").await.unwrap(),
        serde_json::Value::Null
    );
    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn xpath_waits_and_returns_nodes() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/basic.html"), false).await.unwrap();
    let b = tab.xpath("//button[@id='btn']", Duration::ZERO).await.unwrap();
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].text().await.unwrap(), "Click me");
    tab.reload().await.unwrap();
    let late = tab
        .xpath("//div[@id='late']/text()", Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(late.len(), 1);
    assert_eq!(late[0].text().await.unwrap(), "Late element");
    let start = Instant::now();
    assert!(
        tab.xpath("//nothing", Duration::from_millis(300))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(start.elapsed() >= Duration::from_millis(300));
    assert!(matches!(
        tab.xpath("//[", Duration::ZERO).await,
        Err(Error::Js(_))
    ));
    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn frames_urls_and_linked_sources() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/links.html"), false).await.unwrap();
    let tree = tab.frame_tree().await.unwrap();
    assert!(tree.frame.url.ends_with("/links.html"));
    let res = tab.frame_resource_urls().await.unwrap();
    assert!(res.iter().any(|u| u.ends_with("/pixel.png")), "{res:?}");
    let abs = tab.get_all_urls(true).await.unwrap();
    assert!(abs.contains(&format!("{base}/page2.html")), "{abs:?}");
    assert!(abs.contains(&format!("{base}/pixel.png")), "{abs:?}");
    assert!(
        !abs.iter().any(|u| u.contains("mailto") || u.contains("#top")),
        "{abs:?}"
    );
    let raw = tab.get_all_urls(false).await.unwrap();
    assert!(raw.contains(&"#top".to_string()) && raw.contains(&"/page2.html".to_string()));
    let n = tab.linked_sources().await.unwrap().len();
    assert_eq!(n, 7, "4 a/img + link + meta + script");
    let url = tab.inspector_url();
    assert!(
        url.contains(&format!("/devtools/page/{}", tab.target_id())),
        "{url}"
    );
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn download_path_and_download_file() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/basic.html"), false).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("downloads");
    tab.set_download_path(&sub).await.unwrap();
    assert!(sub.is_dir());
    tab.download_file(&format!("{base}/api/data"), Some("data.json"))
        .await
        .unwrap();
    let file = sub.join("data.json");
    let start = Instant::now();
    while !file.is_file() && start.elapsed() < Duration::from_secs(15) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        r#"{"ok":true,"items":[1,2,3]}"#
    );
    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn window_bounds_and_states() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/basic.html"), false).await.unwrap();
    tab.set_window_bounds(20, 30, 900, 700).await.unwrap();
    let (_, b) = tab.get_window().await.unwrap();
    assert_eq!((b.width, b.height), (Some(900), Some(700)));
    assert_eq!(b.window_state, Some(WindowState::Normal));
    tab.maximize().await.unwrap();
    let (_, b) = tab.get_window().await.unwrap();
    assert_eq!(b.window_state, Some(WindowState::Maximized));
    tab.normal().await.unwrap();
    let (_, b) = tab.get_window().await.unwrap();
    assert_eq!(b.window_state, Some(WindowState::Normal));
    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn insecure_warning_bypass_types_the_phrase() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/basic.html"), false).await.unwrap();
    tab.evaluate("window.typed = ''; document.addEventListener('keypress', (e) => window.typed += e.key)")
        .await
        .unwrap();
    tab.bypass_insecure_connection_warning().await.unwrap();
    let typed: String = tab.evaluate_as("window.typed").await.unwrap();
    assert_eq!(typed, "thisisunsafe");
    browser.close().await.unwrap();
}
