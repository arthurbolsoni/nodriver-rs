mod common;

use std::time::Duration;

use nodriver::cdp::network;
use nodriver::{Browser, Error};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn launch_version_close_removes_temp_profile() {
    let browser = common::browser().await;
    let v = browser.version().await.unwrap();
    assert!(
        v.product.contains("Chrome") || v.product.contains("Edg"),
        "{}",
        v.product
    );
    assert!(!v.user_agent.is_empty());
    let profile = browser.user_data_dir().unwrap().to_path_buf();
    assert!(profile.exists());
    browser.close().await.unwrap();
    assert!(
        !profile.exists(),
        "temporary profile left behind: {}",
        profile.display()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn navigate_and_evaluate() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/basic.html"), false).await.unwrap();
    assert_eq!(tab.title().await.unwrap(), "Basic");
    assert_eq!(tab.evaluate("1 + 1").await.unwrap(), json!(2));
    assert_eq!(tab.evaluate("Promise.resolve('ok')").await.unwrap(), json!("ok"));
    assert_eq!(tab.evaluate("undefined").await.unwrap(), json!(null));
    match tab.evaluate("throw new Error('boom')").await {
        Err(Error::Js(msg)) => assert!(msg.contains("boom"), "{msg}"),
        other => panic!("expected Js error, got {other:?}"),
    }
    // Same-document navigation returns without waiting for a load event.
    tab.navigate(&format!("{base}/basic.html#section")).await.unwrap();
    assert!(tab.url().await.unwrap().ends_with("#section"));
    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn navigation_error() {
    let browser = common::browser().await;
    let tab = browser.main_tab().await.unwrap();
    match tab.navigate("http://127.0.0.1:1/").await {
        Err(Error::Navigation(msg)) => assert!(msg.contains("ERR_"), "{msg}"),
        other => panic!("expected navigation error, got {other:?}"),
    }
    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn select_waits_and_query_does_not() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/basic.html"), false).await.unwrap();
    tab.evaluate("document.getElementById('late')?.remove()")
        .await
        .unwrap();
    tab.reload().await.unwrap();
    assert!(tab.query("#late").await.unwrap().is_none());
    let late = tab.select("#late").await.unwrap();
    assert_eq!(late.text().await.unwrap(), "Late element");
    match tab.select_timeout("#never", Duration::from_millis(300)).await {
        Err(Error::NotFound(_)) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
    let options = tab.query_all("#sel option").await.unwrap();
    assert_eq!(options.len(), 2);
    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn find_by_text() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/basic.html"), false).await.unwrap();
    let el = tab.find("find this exact phrase", true).await.unwrap();
    assert_eq!(el.attribute("id").await.unwrap().as_deref(), Some("exact"));
    let all = tab.find_all("Find this exact phrase").await.unwrap();
    assert_eq!(all.len(), 2);
    let input = tab.find("Your name", true).await.unwrap();
    assert_eq!(input.attribute("id").await.unwrap().as_deref(), Some("name"));
    let shadow = tab.find("Shadow button", true).await.unwrap();
    shadow.mouse_click().await.unwrap();
    assert_eq!(shadow.text().await.unwrap(), "Shadow clicked");
    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn clicks() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/basic.html"), false).await.unwrap();
    let btn = tab.select("#btn").await.unwrap();
    btn.mouse_click().await.unwrap();
    btn.click().await.unwrap();
    assert_eq!(tab.evaluate("window.clicks").await.unwrap(), json!(2));
    assert_eq!(
        tab.select("#out").await.unwrap().text().await.unwrap(),
        "clicked 2"
    );
    let r = btn.rect().await.unwrap();
    assert!(r.width > 0.0 && r.height > 0.0);
    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn typing_keys_and_select_option() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/basic.html"), false).await.unwrap();
    let input = tab.select("#name").await.unwrap();
    input.send_keys("Arthur").await.unwrap();
    assert_eq!(input.value().await.unwrap().as_deref(), Some("Arthur"));
    input.clear_input().await.unwrap();
    assert_eq!(input.value().await.unwrap().as_deref(), Some(""));
    input.focus().await.unwrap();
    tab.insert_text("Inserted").await.unwrap();
    tab.press_key("Enter").await.unwrap();
    tab.wait_until(
        Duration::from_secs(3),
        "document.getElementById('submitted').textContent === 'Inserted'",
        "submit",
    )
    .await
    .unwrap();
    let keys: Vec<String> = tab.evaluate_as("window.keys").await.unwrap();
    assert!(keys.contains(&"Enter".to_string()), "{keys:?}");

    let beta = tab.select("#sel option[value=b]").await.unwrap();
    beta.select_option().await.unwrap();
    assert_eq!(tab.select("#chosen").await.unwrap().text().await.unwrap(), "b");
    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn file_upload() {
    let base = common::serve().await;
    let dir = tempfile::tempdir().unwrap();
    let f1 = dir.path().join("one.txt");
    let f2 = dir.path().join("two.txt");
    std::fs::write(&f1, "1").unwrap();
    std::fs::write(&f2, "2").unwrap();
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/basic.html"), false).await.unwrap();
    tab.select("#file")
        .await
        .unwrap()
        .send_file(&[&f1, &f2])
        .await
        .unwrap();
    tab.wait_until(
        Duration::from_secs(3),
        "document.getElementById('files').textContent === 'one.txt,two.txt'",
        "files",
    )
    .await
    .unwrap();
    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn tabs_open_list_close() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let first = browser.get(&format!("{base}/basic.html"), false).await.unwrap();
    let second = browser.get(&format!("{base}/page2.html"), true).await.unwrap();
    assert_eq!(second.title().await.unwrap(), "Second");
    assert_ne!(first.target_id(), second.target_id());
    assert_eq!(browser.tabs().await.unwrap().len(), 2);
    second.activate().await.unwrap();
    second.close().await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let tabs = browser.tabs().await.unwrap();
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0].target_id(), first.target_id());
    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn network_events() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.main_tab().await.unwrap();
    tab.send(&network::EnableParams::new()).await.unwrap();
    let mut requests = tab.subscribe::<network::RequestWillBeSentEvent>();
    let mut raw = tab.subscribe_raw(Some("Network."));
    tab.navigate(&format!("{base}/page2.html")).await.unwrap();
    let mut urls = Vec::new();
    while let Ok(Some(r)) = tokio::time::timeout(Duration::from_secs(3), requests.next()).await {
        urls.push(r.request.url.clone());
        if r.request.url.ends_with("/api/data") {
            break;
        }
    }
    assert!(urls.iter().any(|u| u.ends_with("/api/data")), "{urls:?}");
    let first = raw.next().await.unwrap();
    assert!(first.method.starts_with("Network."));
    assert_eq!(first.session_id, tab.session_id());
    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_to_running_browser() {
    let base = common::serve().await;
    let launched = common::browser().await;
    launched.get(&format!("{base}/basic.html"), false).await.unwrap();
    let attached = Browser::connect("127.0.0.1", launched.port()).await.unwrap();
    let tabs = attached.tabs().await.unwrap();
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0].title().await.unwrap(), "Basic");
    launched.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn profile_in_use() {
    let dir = tempfile::tempdir().unwrap();
    let first = Browser::launch(common::config().user_data_dir(dir.path()))
        .await
        .unwrap();
    match Browser::launch(common::config().user_data_dir(dir.path())).await {
        Err(Error::ProfileInUse(p)) => assert_eq!(p, dir.path()),
        Ok(b) => {
            b.close().await.unwrap();
            panic!("second launch on the same profile succeeded");
        }
        Err(e) => panic!("expected ProfileInUse, got {e}"),
    }
    first.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_pid_known_and_gone_after_close() {
    let browser = common::browser().await;
    let pid = browser.pid().expect("browser pid from SystemInfo.getProcessInfo");
    browser.close().await.unwrap();
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\"")),
        "pid {pid} still running"
    );
}
