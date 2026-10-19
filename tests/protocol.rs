#[path = "../src/wire.rs"]
mod wire;
use std::{
    io,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use wire::Value;

struct Service {
    child: Child,
    input: Option<ChildStdin>,
    messages: mpsc::Receiver<io::Result<Value>>,
}
impl Service {
    fn open(root: &std::path::Path, database: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_knotify"))
            .arg("catalog")
            .arg("--root")
            .arg(root)
            .arg("--database")
            .arg(database)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let mut output = child.stdout.take().unwrap();
        let (tx, messages) = mpsc::channel();
        thread::spawn(move || {
            loop {
                match wire::read(&mut output) {
                    Ok(Some(v)) => {
                        if tx.send(Ok(v)).is_err() {
                            return;
                        }
                    }
                    Ok(None) => return,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                }
            }
        });
        Self {
            child,
            input,
            messages,
        }
    }
    fn read(&self) -> Value {
        self.messages
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap()
    }
    fn send(&mut self, op: &str, id: i64) {
        let v = Value::map([
            ("version", 1.into()),
            ("id", id.into()),
            ("op", op.into()),
            ("path", Vec::<u8>::new().into()),
        ]);
        wire::write(self.input.as_mut().unwrap(), &v).unwrap();
    }
    fn close(&mut self) {
        self.input.take();
        let started = Instant::now();
        loop {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "port did not exit on EOF"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn catalog_batches_wait_for_acknowledgement_and_eof_stops_blocked_work() {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    for i in 0..300 {
        std::fs::write(root.path().join(format!("file-{i}")), b"x").unwrap();
    }
    let mut service = Service::open(root.path(), &data.path().join("catalog.sqlite3"));
    assert_eq!(
        service.read().get("type").unwrap().bytes().unwrap(),
        b"ready"
    );
    service.send("reconcile_dir", 1);
    let batch = service.read();
    assert_eq!(batch.get("id").unwrap().int().unwrap(), 1);
    assert_eq!(batch.get("type").unwrap().bytes().unwrap(), b"batch");
    assert!(batch.get("items").unwrap().list().unwrap().len() <= 128);
    assert!(
        service
            .messages
            .recv_timeout(Duration::from_millis(150))
            .is_err()
    );
    service.send("ack", 1);
    assert_eq!(
        service.read().get("type").unwrap().bytes().unwrap(),
        b"batch"
    );
    service.close();
}
