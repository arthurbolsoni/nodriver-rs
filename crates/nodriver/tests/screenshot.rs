mod common;

use nodriver::ImageFormat;

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

fn png_size(png: &[u8]) -> (u32, u32) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
    (
        u32::from_be_bytes(png[16..20].try_into().unwrap()),
        u32::from_be_bytes(png[20..24].try_into().unwrap()),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn full_page_jpeg_and_element_screenshots() {
    let base = common::serve().await;
    let browser = common::browser().await;
    let tab = browser.get(&format!("{base}/long.html"), false).await.unwrap();
    let dpr: f64 = tab.evaluate_as("devicePixelRatio").await.unwrap();
    let inner: f64 = tab.evaluate_as("innerHeight").await.unwrap();

    let full = tab.screenshot(ImageFormat::Png, true).await.unwrap();
    let (_, h) = png_size(&full);
    assert_eq!(h, (3000.0 * dpr).round() as u32);
    assert!(h as f64 > inner * dpr);
    // The page itself did not change size or scroll.
    assert_eq!(tab.evaluate_as::<f64>("innerHeight").await.unwrap(), inner);

    let jpg = tab.screenshot(ImageFormat::Jpeg, false).await.unwrap();
    assert_eq!(&jpg[..3], b"\xff\xd8\xff", "not a JPEG");

    let el = tab.select("#box").await.unwrap();
    let shot = el.screenshot(ImageFormat::Png, 1.0).await.unwrap();
    assert_eq!(png_size(&shot), ((200.0 * dpr) as u32, (100.0 * dpr) as u32));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub").join("box.jpg");
    el.save_screenshot(&path).await.unwrap();
    assert_eq!(&std::fs::read(&path).unwrap()[..3], b"\xff\xd8\xff");
    let page = dir.path().join("page.png");
    tab.save_screenshot(&page, true).await.unwrap();
    assert_eq!(png_size(&std::fs::read(&page).unwrap()).1, h);
    browser.close().await.unwrap();
}
