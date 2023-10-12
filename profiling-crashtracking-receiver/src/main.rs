use chrono::Utc;
use std::fs::File;
use std::io::prelude::*;
use std::path::Path;
use uuid::Uuid;

/// Recieves data on stdin, and forwards it to somewhere its useful
/// For now, just sent to a file.
/// Future enhancement: set of key/value pairs sent over pipe to setup
/// Future enhancement: publish to DD endpoint
pub fn main() -> anyhow::Result<()> {
    let uuid = Uuid::new_v4();
    let now = Utc::now().to_rfc3339();
    let path = format!("{now}.txt");
    let path = Path::new(&path);
    let mut file = File::create(path)?;
    let stdin = std::io::stdin();
    writeln!(file, "{uuid}")?;
    for (i, line) in stdin.lock().lines().enumerate() {
        let line = line?;
        writeln!(file, "{i} {}", line)?;
    }
    Ok(())
}
