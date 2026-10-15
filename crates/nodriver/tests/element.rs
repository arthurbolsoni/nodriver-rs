mod common;

use nodriver::cdp::input::MouseButton;
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn drag_to_element_by_offset_and_point() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/drag.html"), false).await.unwrap();
    let handle = tab.select("#handle").await.unwrap();
    let target = tab.select("#target").await.unwrap();
    handle.mouse_drag_to_element(&target, 10).await.unwrap();
    assert_eq!(tab.evaluate("window.dropped").await.unwrap(), json!(true));
    let moves: u64 = tab.evaluate_as("window.moves").await.unwrap();
    assert!(moves >= 10, "moves {moves}");

    let before = handle.rect().await.unwrap();
    handle.mouse_drag_by(-100.0, 0.0, 1).await.unwrap();
    let after = handle.rect().await.unwrap();
    assert!(
        (after.left - (before.left - 100.0)).abs() < 1.0,
        "{before:?} -> {after:?}"
    );
    assert_eq!(tab.evaluate("window.dropped").await.unwrap(), json!(false));

    handle.mouse_drag_to(80.0, 80.0, 5).await.unwrap();
    let r = handle.rect().await.unwrap();
    let (cx, cy) = r.center();
    assert!((cx - 80.0).abs() < 1.0 && (cy - 80.0).abs() < 1.0, "{r:?}");

    tab.mouse_drag((80.0, 80.0), (350.0, 250.0), 3).await.unwrap();
    assert_eq!(tab.evaluate("window.dropped").await.unwrap(), json!(true));

    handle.mouse_click_with(MouseButton::Right, 2).await.unwrap();
    assert_eq!(
        tab.evaluate("window.context").await.unwrap(),
        json!({ "button": 2, "ctrl": true })
    );
    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn text_attributes_tree_and_values() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/links.html"), false).await.unwrap();
    let ul = tab.select("#ul").await.unwrap();
    assert_eq!(ul.text_all().await.unwrap(), "one two bold three");
    let attrs = ul.attributes().await.unwrap();
    assert_eq!(attrs.get("id").map(String::as_str), Some("ul"));
    assert_eq!(attrs.get("data-kind").map(String::as_str), Some("list"));
    ul.set_attribute("data-kind", "changed").await.unwrap();
    assert_eq!(
        ul.attribute("data-kind").await.unwrap().as_deref(),
        Some("changed")
    );
    ul.remove_attribute("data-kind").await.unwrap();
    assert_eq!(ul.attribute("data-kind").await.unwrap(), None);

    let kids = ul.children().await.unwrap();
    assert_eq!(kids.len(), 3);
    assert_eq!(kids[0].tag().await.unwrap(), "li");
    let parent = kids[2].parent().await.unwrap().unwrap();
    assert_eq!(parent.attribute("id").await.unwrap().as_deref(), Some("ul"));
    let html = tab.select("html").await.unwrap();
    assert!(html.parent().await.unwrap().is_none());

    kids[0].set_text("uno").await.unwrap();
    assert_eq!(kids[0].text().await.unwrap(), "uno");
    assert!(kids[1].set_text("x").await.is_err(), "li with two children");
    kids[2].remove().await.unwrap();
    assert_eq!(ul.children().await.unwrap().len(), 2);

    let inp = tab.select("#inp").await.unwrap();
    inp.set_value("hello").await.unwrap();
    assert_eq!(inp.value().await.unwrap().as_deref(), Some("hello"));
    assert_eq!(
        tab.evaluate("[window.inputs, window.changes]").await.unwrap(),
        json!([1, 1])
    );
    inp.call_method("blur").await.unwrap();

    let a = tab.select("#a1").await.unwrap();
    tab.evaluate("window.scrollTo(0, 0)").await.unwrap();
    let r = a.document_rect().await.unwrap();
    assert_eq!(r, a.rect().await.unwrap());
    browser.close().await.unwrap();
}
