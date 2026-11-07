//! Helpers shared by the collector tests. Test-only: nothing here is compiled
//! into the binary.

use std::path::PathBuf;
use std::process::Command;

/// Kills the process whose pid a test's shell wrote to the file, when dropped.
///
/// A test that leaves a grandchild holding the launcher's pipes (`sleep N &`) must
/// not leave it running when an assertion fails. The pid comes from a file the
/// shell wrote itself, never from the output under test: if the code that reads
/// that output is the thing that is broken, the pid would be lost with it.
pub struct KillGrandchild(pub PathBuf);

impl Drop for KillGrandchild {
    fn drop(&mut self) {
        if let Ok(pid) = std::fs::read_to_string(&self.0) {
            let pid = pid.trim();
            if !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()) {
                let _ = Command::new("sh")
                    .arg("-c")
                    .arg(format!("kill {pid}"))
                    .status();
            }
        }
    }
}
