mod common;

#[tokio::test(flavor = "multi_thread")]
async fn screenshot_is_png_of_viewport_size() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/basic.html"), false).await.unwrap();
    let png = tab.screenshot_png().await.unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
    assert_eq!(&png[12..16], b"IHDR");
    let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
    let (vw, vh): (f64, f64) = tab
        .evaluate_as("[innerWidth * devicePixelRatio, innerHeight * devicePixelRatio]")
        .await
        .map(|v: Vec<f64>| (v[0], v[1]))
        .unwrap();
    assert_eq!(width, vw.round() as u32);
    assert_eq!(height, vh.round() as u32);
    browser.close().await.unwrap();
}
