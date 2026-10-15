mod common;

use nodriver::cdp::network::CookieParam;
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn local_storage_get_and_set() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/basic.html"), false).await.unwrap();
    tab.evaluate("localStorage.setItem('from_js', '1')")
        .await
        .unwrap();
    tab.set_local_storage([("a", "x"), ("b", "y z")]).await.unwrap();
    assert_eq!(
        tab.evaluate("[localStorage.getItem('a'), localStorage.getItem('b')]")
            .await
            .unwrap(),
        json!(["x", "y z"])
    );
    let items = tab.local_storage().await.unwrap();
    assert_eq!(items.get("from_js").map(String::as_str), Some("1"));
    assert_eq!(items.get("b").map(String::as_str), Some("y z"));
    assert_eq!(items.len(), 3);
    let blank = browser.new_tab("about:blank").await.unwrap();
    assert!(blank.local_storage().await.is_err());
    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn cookies_get_set_clear_save_load() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/basic.html"), false).await.unwrap();
    tab.evaluate("document.cookie = 'k1=v1; max-age=3600'; document.cookie = 'k2=v2'")
        .await
        .unwrap();
    let jar = browser.cookies();
    let names = |cs: &[nodriver::cdp::network::Cookie]| {
        let mut n: Vec<String> = cs.iter().map(|c| c.name.clone()).collect();
        n.sort();
        n
    };
    assert_eq!(names(&jar.get_all().await.unwrap()), ["k1", "k2"]);

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("cookies.json");
    assert_eq!(jar.save(&file).await.unwrap(), 2);
    let only_k1 = dir.path().join("k1.json");
    assert_eq!(jar.save_filtered(&only_k1, |c| c.name == "k1").await.unwrap(), 1);

    jar.clear().await.unwrap();
    assert!(jar.get_all().await.unwrap().is_empty());
    assert_eq!(tab.evaluate("document.cookie").await.unwrap(), json!(""));

    assert_eq!(jar.load(&file).await.unwrap(), 2);
    let back = jar.get_all().await.unwrap();
    assert_eq!(names(&back), ["k1", "k2"]);
    let k1 = back.iter().find(|c| c.name == "k1").unwrap();
    assert!(!k1.session && k1.expires.unwrap() > 0.0, "k1 kept its expiry");
    assert!(back.iter().find(|c| c.name == "k2").unwrap().session);

    jar.clear().await.unwrap();
    assert_eq!(jar.load_filtered(&file, |c| c.name == "k2").await.unwrap(), 1);
    assert_eq!(names(&jar.get_all().await.unwrap()), ["k2"]);

    let mut p = CookieParam {
        name: "k3".into(),
        value: "v3".into(),
        url: Some(format!("{base}/")),
        domain: None,
        path: None,
        secure: None,
        http_only: None,
        same_site: None,
        expires: None,
        priority: None,
        source_scheme: None,
        source_port: None,
        partition_key: None,
    };
    p.http_only = Some(true);
    jar.set_all(vec![p]).await.unwrap();
    assert_eq!(names(&jar.get_all().await.unwrap()), ["k2", "k3"]);
    // http-only: not visible to the page.
    assert_eq!(tab.evaluate("document.cookie").await.unwrap(), json!("k2=v2"));
    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn grant_all_permissions_grants_geolocation() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/basic.html"), false).await.unwrap();
    let q = "navigator.permissions.query({name: 'geolocation'}).then((p) => p.state)";
    assert_ne!(tab.evaluate(q).await.unwrap(), json!("granted"));
    let granted = browser.grant_all_permissions().await.unwrap();
    assert!(granted.iter().any(|p| p == "geolocation"), "{granted:?}");
    assert_eq!(tab.evaluate(q).await.unwrap(), json!("granted"));
    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn create_context_is_isolated_and_takes_a_proxy() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/basic.html"), false).await.unwrap();
    tab.evaluate("document.cookie = 'main=1'; localStorage.setItem('main', '1')")
        .await
        .unwrap();

    let other = browser
        .create_context(&format!("{base}/basic.html"), None, None)
        .await
        .unwrap();
    assert_eq!(other.title().await.unwrap(), "Basic");
    assert_eq!(
        other
            .evaluate("[document.cookie, localStorage.getItem('main')]")
            .await
            .unwrap(),
        json!(["", null])
    );
    other.close().await.unwrap();

    // The test server answers proxy requests, so a host that does not resolve still loads.
    let proxy = base.clone();
    let proxied = browser
        .create_context("http://site.invalid/page2.html", Some(&proxy), None)
        .await
        .unwrap();
    assert_eq!(proxied.title().await.unwrap(), "Second");
    assert_eq!(proxied.url().await.unwrap(), "http://site.invalid/page2.html");
    browser.close().await.unwrap();
}
