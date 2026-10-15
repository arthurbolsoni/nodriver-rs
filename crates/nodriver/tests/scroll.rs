mod common;

use std::time::Duration;

#[tokio::test(flavor = "multi_thread")]
async fn scroll_gesture_loads_infinite_list_until_bottom() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/scroll.html"), false).await.unwrap();
    let count = || async {
        tab.evaluate_as::<u64>("document.querySelectorAll('.item').length")
            .await
            .unwrap()
    };
    assert_eq!(count().await, 20);
    assert!(!tab.scroll_bottom_reached().await.unwrap());

    let mut rounds = 0;
    loop {
        tab.scroll_down(100.0).await.unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        rounds += 1;
        if tab.scroll_bottom_reached().await.unwrap() || rounds > 60 {
            break;
        }
    }
    assert!(
        tab.scroll_bottom_reached().await.unwrap(),
        "bottom not reached in {rounds} rounds"
    );
    assert_eq!(count().await, 80);
    let end: String = tab
        .evaluate_as("document.getElementById('sentinel').textContent")
        .await
        .unwrap();
    assert_eq!(end, "end");

    let y: f64 = tab.evaluate_as("window.scrollY").await.unwrap();
    tab.scroll_up(50.0).await.unwrap();
    let y2: f64 = tab.evaluate_as("window.scrollY").await.unwrap();
    assert!(y2 < y, "scroll_up did not move: {y} -> {y2}");
    assert!(!tab.scroll_bottom_reached().await.unwrap());
    browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn element_scroll_moves_its_own_content() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/scroll.html"), false).await.unwrap();
    let boxel = tab.select("#box").await.unwrap();
    boxel.scroll_down(100.0).await.unwrap();
    let (inner, page): (f64, f64) = tab
        .evaluate_as("[document.getElementById('box').scrollTop, window.scrollY]")
        .await
        .map(|v: Vec<f64>| (v[0], v[1]))
        .unwrap();
    assert!(inner > 100.0, "box scrollTop {inner}");
    assert_eq!(page, 0.0, "the page scrolled too");
    boxel.scroll_up(100.0).await.unwrap();
    let inner2: f64 = tab
        .evaluate_as("document.getElementById('box').scrollTop")
        .await
        .unwrap();
    assert!(inner2 < inner);
    browser.close().await.unwrap();
}
