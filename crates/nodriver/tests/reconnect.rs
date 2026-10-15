mod common;

use std::time::Duration;

use nodriver::cdp::page;
use nodriver::{Error, RECONNECTED_EVENT};

#[tokio::test(flavor = "multi_thread")]
async fn reconnects_and_keeps_tabs_and_subscriptions() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let conn = browser.connection().clone();
    let tab = browser.get(&format!("{base}/basic.html"), false).await.unwrap();
    let old_session = tab.session_id().unwrap();
    let generation = conn.generation();
    let mut reconnected = conn.subscribe_raw(Some(RECONNECTED_EVENT), None);
    let mut loads = tab.subscribe::<page::LoadEventFiredEvent>();

    conn.disconnect();
    let ev = tokio::time::timeout(Duration::from_secs(10), reconnected.next())
        .await
        .expect("reconnected event in time")
        .expect("stream open");
    assert_eq!(ev.params["generation"], generation + 1);
    assert_eq!(conn.generation(), generation + 1);
    assert!(!conn.is_closed());

    // Same tab, new session.
    assert_eq!(tab.title().await.unwrap(), "Basic");
    let new_session = tab.session_id().unwrap();
    assert_ne!(new_session, old_session);

    // Page is enabled again in the new session; the subscription made before the drop
    // receives the load event of the new session.
    tab.reload().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), loads.next())
        .await
        .expect("load event after reconnection")
        .expect("stream open");

    browser.close().await.unwrap();
    match tab.evaluate("1").await {
        Err(Error::Disconnected) => {}
        other => panic!("expected Disconnected after close, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pending_call_fails_and_is_not_resent() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/basic.html"), false).await.unwrap();
    tab.evaluate("window.runs = 0").await.unwrap();
    let slow = {
        let tab = tab.clone();
        tokio::spawn(async move {
            tab.evaluate("window.runs++, new Promise(r => setTimeout(() => r('done'), 1500))")
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    browser.connection().disconnect();
    match slow.await.unwrap() {
        Err(Error::Disconnected) => {}
        other => panic!("expected Disconnected for the pending call, got {other:?}"),
    }
    assert_eq!(tab.evaluate("window.runs").await.unwrap(), serde_json::json!(1));
    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_gone_ends_streams() {
    let browser = common::browser().await;
    let tab = browser.main_tab().await.unwrap();
    let mut events = tab.subscribe_raw(None);
    let pid = browser.pid().unwrap();
    #[cfg(windows)]
    std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .output()
        .unwrap();
    #[cfg(not(windows))]
    std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .output()
        .unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(15), async {
        while events.next().await.is_some() {}
    })
    .await;
    assert!(ended.is_ok(), "event stream did not end after the browser died");
    match tab.evaluate("1").await {
        Err(Error::Disconnected) => {}
        other => panic!("expected Disconnected, got {other:?}"),
    }
    browser.close().await.unwrap();
}
