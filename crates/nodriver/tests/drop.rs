//! A launched `Browser` dropped without `close` stops its process and deletes its temporary
//! profile; a user profile is kept and an attached browser keeps running.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use nodriver::Browser;

fn assert_stopped(pid: u32, profile: &std::path::Path) {
    assert!(!common::process_alive(pid), "browser pid {pid} still running");
    assert!(
        !profile.exists(),
        "temporary profile left behind: {}",
        profile.display()
    );
}

#[test]
fn panic_in_thread_stops_browser_and_removes_temp_profile() {
    let (tx, rx) = std::sync::mpsc::channel::<(u32, PathBuf)>();
    let thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let browser = common::browser().await;
            let pid = browser.pid().expect("browser pid");
            let profile = browser.user_data_dir().unwrap().to_path_buf();
            assert!(common::process_alive(pid));
            assert!(profile.exists());
            eprintln!(
                "before panic: pid {pid} alive {}, profile {} exists {}",
                common::process_alive(pid),
                profile.display(),
                profile.exists()
            );
            tx.send((pid, profile)).unwrap();
            panic!("deliberate panic before close");
        });
    });
    assert!(thread.join().is_err(), "the thread should have panicked");
    let (pid, profile) = rx.recv().unwrap();
    eprintln!(
        "after panic: pid {pid} alive {}, profile {} exists {}",
        common::process_alive(pid),
        profile.display(),
        profile.exists()
    );
    assert_stopped(pid, &profile);
}

#[tokio::test(flavor = "multi_thread")]
async fn panic_in_task_stops_browser_and_removes_temp_profile() {
    let (tx, rx) = tokio::sync::oneshot::channel::<(u32, PathBuf)>();
    let task = tokio::spawn(async move {
        let browser = common::browser().await;
        let tab = browser.main_tab().await.unwrap();
        tab.navigate("about:blank").await.unwrap();
        tx.send((
            browser.pid().expect("browser pid"),
            browser.user_data_dir().unwrap().to_path_buf(),
        ))
        .unwrap();
        panic!("deliberate panic before close");
    });
    assert!(task.await.unwrap_err().is_panic());
    let (pid, profile) = rx.await.unwrap();
    assert_stopped(pid, &profile);
}

#[tokio::test(flavor = "multi_thread")]
async fn launch_with_dropped_without_close() {
    let browser = Browser::launch_with(common::config(), |exe, args| {
        std::process::Command::new(exe)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| ())
    })
    .await
    .unwrap();
    let pid = browser.pid().expect("browser pid");
    let profile = browser.user_data_dir().unwrap().to_path_buf();
    drop(browser);
    assert_stopped(pid, &profile);
}

#[tokio::test(flavor = "multi_thread")]
async fn shared_browser_stops_on_last_drop() {
    let first = Arc::new(common::browser().await);
    let second = first.clone();
    let pid = first.pid().expect("browser pid");
    let profile = first.user_data_dir().unwrap().to_path_buf();
    drop(first);
    assert!(common::process_alive(pid));
    assert!(profile.exists());
    second.version().await.unwrap();
    drop(second);
    assert_stopped(pid, &profile);
}

#[tokio::test(flavor = "multi_thread")]
async fn drop_keeps_user_profile() {
    let dir = tempfile::tempdir().unwrap();
    let browser = Browser::launch(common::config().user_data_dir(dir.path()))
        .await
        .unwrap();
    let pid = browser.pid().expect("browser pid");
    drop(browser);
    assert!(!common::process_alive(pid), "browser pid {pid} still running");
    assert!(
        std::fs::read_dir(dir.path()).unwrap().next().is_some(),
        "user profile emptied"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_attached_browser_leaves_it_running() {
    let launched = common::browser().await;
    let pid = launched.pid().expect("browser pid");
    let attached = Browser::connect("127.0.0.1", launched.port()).await.unwrap();
    assert_eq!(attached.pid(), Some(pid));
    drop(attached);
    assert!(common::process_alive(pid));
    launched.version().await.unwrap();
    launched.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn drop_after_close_is_a_no_op() {
    let browser = common::browser().await;
    let profile = browser.user_data_dir().unwrap().to_path_buf();
    browser.close().await.unwrap();
    assert!(!profile.exists());
    let start = std::time::Instant::now();
    drop(browser);
    assert!(start.elapsed() < std::time::Duration::from_millis(500));
}
