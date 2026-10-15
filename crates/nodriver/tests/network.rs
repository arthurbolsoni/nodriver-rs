mod common;

use std::time::Duration;

use serde_json::json;

/// Concurrent `Browser::tab` calls for one target share a single session.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_attach_shares_one_session() {
    let browser = common::browser().await;
    let id = browser
        .connection()
        .call("Target.createTarget", json!({ "url": "about:blank" }), None)
        .await
        .unwrap()["targetId"]
        .as_str()
        .unwrap()
        .to_string();
    let (a, b) = tokio::join!(browser.tab(&id), browser.tab(&id));
    assert_eq!(a.unwrap().session_id(), b.unwrap().session_id());
    browser.close().await.unwrap();
}

/// Response bodies stay available through `Network.getResponseBody` after `loadingFinished`,
/// including when `Network` was enabled before a cross-process navigation.
#[tokio::test(flavor = "multi_thread")]
async fn response_body_after_loading_finished() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.main_tab().await.unwrap();
    tab.call("Network.enable", json!({})).await.unwrap();
    tab.navigate(&format!("{base}/basic.html")).await.unwrap();
    let mut events = tab.subscribe_raw(Some("Network."));
    tab.evaluate("fetch('/page2.html').then(r => r.text()).then(t => t.length)")
        .await
        .unwrap();
    let id = tokio::time::timeout(Duration::from_secs(5), async {
        let mut id = None;
        while let Some(ev) = events.next().await {
            if ev.method == "Network.requestWillBeSent"
                && ev.params["request"]["url"]
                    .as_str()
                    .is_some_and(|u| u.ends_with("/page2.html"))
            {
                id = ev.params["requestId"].as_str().map(str::to_string);
            }
            if ev.method == "Network.loadingFinished" && id.as_deref() == ev.params["requestId"].as_str() {
                return id.unwrap();
            }
        }
        panic!("no loadingFinished")
    })
    .await
    .unwrap();
    let body = tab
        .call("Network.getResponseBody", json!({ "requestId": id }))
        .await
        .unwrap();
    assert!(body["body"].as_str().unwrap().contains("second page"), "{body}");
    browser.close().await.unwrap();
}
