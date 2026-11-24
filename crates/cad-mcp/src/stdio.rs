//! Newline-delimited JSON-RPC over stdio.

use std::io::{BufRead, Error, ErrorKind, Read, Write};

use crate::server::McpServer;

const MAX_STDIO_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Read one JSON-RPC message per line and write one response per request.
///
/// Notifications produce no output. EOF ends the loop. Lines exceeding 16 MiB
/// are rejected with [`ErrorKind::InvalidData`] to prevent unbounded OOM reads.
pub fn serve_stdio<R: BufRead, W: Write>(
    server: &mut McpServer,
    mut reader: R,
    mut writer: W,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let bytes_read = (&mut reader)
            .take((MAX_STDIO_LINE_BYTES + 1) as u64)
            .read_until(b'\n', &mut buf)?;
        if bytes_read == 0 {
            break;
        }
        if buf.len() > MAX_STDIO_LINE_BYTES {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("JSON-RPC stdio line exceeds {MAX_STDIO_LINE_BYTES} byte limit"),
            ));
        }
        let line = std::str::from_utf8(&buf)
            .map_err(|err| Error::new(ErrorKind::InvalidData, err))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(response) = server.handle_message(trimmed) {
            writeln!(writer, "{response}")?;
            writer.flush()?;
        }
    }
    Ok(())
}
