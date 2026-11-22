use super::{Session, cmd};
use std::fs::OpenOptions;
use std::net::Shutdown;
use std::path::PathBuf;

#[test]
fn disconnected_named_stdout_pipe_ends_quietly() {
    let session = Session::new();
    let pipe = PathBuf::from(format!(r"\\.\pipe\hc-stdout-test-{}", std::process::id()));
    let listener = herdr_core::platform::listen(&pipe).unwrap();
    let writer = OpenOptions::new().read(true).write(true).open(&pipe).unwrap();
    let reader = listener.incoming().next().unwrap().unwrap();
    reader.shutdown(Shutdown::Both).unwrap();

    let out = cmd(&session.name, &session.root).arg("--help").stdout(writer).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(out.stderr.is_empty(), "{stderr}");
}
