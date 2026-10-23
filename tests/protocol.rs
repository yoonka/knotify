#[path = "../src/wire.rs"]
#[allow(dead_code)]
mod wire;
use std::{
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
#[test]
fn hint_service_reports_readiness_and_exits_when_its_owner_closes() {
    let root = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_knotify"))
        .args(["--protocol", "etf", "--recursive", "--path"])
        .arg(root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let guarded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let ready = wire::read(child.stdout.as_mut().unwrap()).unwrap().unwrap();
        assert_eq!(ready.get("type").unwrap().bytes().unwrap(), b"ready");
        child.stdin.take();
        let started = Instant::now();
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "watcher did not exit on EOF"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }));
    let _ = child.kill();
    let _ = child.wait();
    guarded.unwrap();
}
