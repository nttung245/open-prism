//! Helpers shared by unit tests.
#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Writes an executable shell script.
///
/// The file is written by a child process: if this (multi-threaded) test process held it open
/// for writing, a `fork` in another test could inherit that descriptor and running the script
/// would fail with "Text file busy" (ETXTBSY).
pub fn write_script(path: &Path, body: &str) {
    let mut child = Command::new("/bin/sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("#!/bin/sh\n{body}\n").as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
}
