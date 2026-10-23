//! Run only in an isolated compositor: changes its clipboard to fixed test text.
#[path = "../src/clipboard.rs"]
mod clipboard;
#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use std::{
        sync::{
            atomic::Ordering::{Acquire, Release},
            Arc,
        },
        time::{Duration, Instant},
    };
    let control = Arc::new(uu_core::clipboard::Clipboard::default());
    let mut local = arboard::Clipboard::new()?;
    local.set_text("uu-local-test-中文;!".to_owned())?;
    control.enabled.store(true, Release);
    control.supported.store(true, Release);
    control.active.store(true, Release);
    clipboard::start(control.clone(), 0)?;
    let wait = |condition: &dyn Fn() -> bool| -> anyhow::Result<()> {
        let end = Instant::now() + Duration::from_secs(4);
        while Instant::now() < end {
            if condition() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        anyhow::bail!("clipboard condition timed out");
    };
    wait(&|| {
        control
            .local
            .lock()
            .unwrap()
            .as_ref()
            .and_then(uu_core::clipboard::Content::as_text)
            == Some("uu-local-test-中文;!")
    })?;
    control.local.lock().unwrap().take();
    *control.remote.lock().unwrap() = Some("uu-remote-test-标点[]{}".into());
    let end = Instant::now() + Duration::from_secs(4);
    loop {
        if local.get_text().ok().as_deref() == Some("uu-remote-test-标点[]{}") {
            break;
        }
        anyhow::ensure!(
            Instant::now() < end,
            "remote text did not reach local clipboard"
        );
        std::thread::sleep(Duration::from_millis(40));
    }
    std::thread::sleep(Duration::from_millis(250));
    anyhow::ensure!(
        control.local.lock().unwrap().is_none(),
        "remote clipboard echoed back"
    );
    control.read_only.store(true, Release);
    local.set_text("uu-read-only-test".to_owned())?;
    std::thread::sleep(Duration::from_millis(250));
    anyhow::ensure!(
        control.local.lock().unwrap().is_none(),
        "view-only clipboard sent local text"
    );
    control.active.store(false, Release);
    *control.remote.lock().unwrap() = Some("uu-inactive-test".into());
    std::thread::sleep(Duration::from_millis(250));
    anyhow::ensure!(
        local.get_text()? == "uu-read-only-test",
        "inactive session changed clipboard"
    );
    anyhow::ensure!(
        control.available.load(Acquire),
        "clipboard backend unavailable"
    );
    control.stopped.store(true, Release);
    println!("local_to_remote=true remote_to_local=true echo=false read_only=true inactive=true");
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("Linux-only isolated clipboard probe");
}
