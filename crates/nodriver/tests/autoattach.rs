mod common;

use std::time::{Duration, Instant};

use serde_json::json;

/// A cross-site iframe auto-attached with `waitForDebuggerOnStart` is adopted: `Browser::tab`
/// reuses its session, `Network` enabled before it runs sees its first request, and the session
/// is forgotten when the iframe goes away.
#[tokio::test(flavor = "multi_thread")]
async fn adopt_auto_attached_iframe() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let conn = browser.connection().clone();
    let tab = browser.main_tab().await.unwrap();
    tab.navigate(&format!("{base}/basic.html")).await.unwrap();

    let mut attached = tab.subscribe_raw(Some("Target.attachedToTarget"));
    tab.call(
        "Target.setAutoAttach",
        json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
    )
    .await
    .unwrap();
    // Another site: 127.0.0.1 → localhost runs in its own process (its own target).
    let child_url = format!("{}/child.html", base.replace("127.0.0.1", "localhost"));
    tab.evaluate(&format!(
        "(() => {{ const f = document.createElement('iframe'); f.src = {}; document.body.append(f); }})()",
        serde_json::to_string(&child_url).unwrap()
    ))
    .await
    .unwrap();

    let ev = loop {
        let ev = tokio::time::timeout(Duration::from_secs(10), attached.next())
            .await
            .expect("no Target.attachedToTarget")
            .unwrap();
        if ev.params["targetInfo"]["type"] == "iframe" {
            break ev;
        }
    };
    assert_eq!(ev.params["waitingForDebugger"], true);
    let target = ev.params["targetInfo"]["targetId"].as_str().unwrap().to_string();
    let session = ev.params["sessionId"].as_str().unwrap().to_string();
    assert!(conn.adopt(&target, &session));
    assert!(!conn.adopt(&target, "another"));

    let mut responses = conn.subscribe_raw(Some("Network.responseReceived"), Some(&session));
    conn.call("Network.enable", json!({}), Some(&session))
        .await
        .unwrap();
    conn.call("Runtime.runIfWaitingForDebugger", json!({}), Some(&session))
        .await
        .unwrap();
    let mut urls = Vec::new();
    while !urls.iter().any(|u: &String| u.ends_with("/api/data")) {
        let ev = tokio::time::timeout(Duration::from_secs(10), responses.next())
            .await
            .unwrap_or_else(|_| panic!("no /api/data response in the iframe session; saw {urls:?}"))
            .unwrap();
        urls.push(ev.params["response"]["url"].as_str().unwrap_or("").to_string());
    }

    let child = browser.tab(&target).await.unwrap();
    assert_eq!(child.session_id().as_deref(), Some(session.as_str()));
    // Listing the tabs keeps the cached iframe and its session.
    browser.tabs().await.unwrap();
    assert_eq!(conn.session_of(&target).as_deref(), Some(session.as_str()));
    child
        .wait_until(
            Duration::from_secs(10),
            "document.getElementById('out')?.textContent === 'ok 3'",
            "fetch result",
        )
        .await
        .unwrap();
    assert_eq!(
        child.evaluate("location.hostname").await.unwrap(),
        json!("localhost")
    );

    tab.evaluate("document.querySelector('iframe').remove()")
        .await
        .unwrap();
    let start = Instant::now();
    while conn.session_of(&target).is_some() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "session kept after the iframe was removed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    browser.close().await.unwrap();
}
