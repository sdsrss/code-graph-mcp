//! Newline-framed stdio message reading, shared by the two JSON-RPC serve loops.
//!
//! Lives in `utils` rather than in `mcp::protocol` because both consumers are
//! top surfaces: the full server loop in `main.rs` and the non-project stub in
//! `cli::serve_non_project_stub`. `src/cli -> crate::mcp` is a forbidden edge
//! (`tests/hardening.rs` FORBIDDEN_EDGES) — the two published surfaces must not
//! borrow from each other, and shared machinery goes in a neutral module, the
//! same way shared symbol resolution went to `resolve.rs`.

use std::io::{BufRead, Read};

/// Upper bound on a single newline-framed stdio message, in bytes.
pub const MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024; // 10MB

/// One framed read from a stdio peer, as produced by [`read_frame`].
#[derive(Debug)]
pub enum StdioFrame {
    /// A complete line, lossily decoded. Never carries a UTF-8 error.
    Message(String),
    /// The line exceeded [`MAX_MESSAGE_SIZE`]. Its tail has already been drained,
    /// so the next `read_frame` starts on a real message boundary. Carries the
    /// byte count read before the cap so the caller can name it in an error.
    Oversized(usize),
    /// The peer closed the stream.
    Eof,
}

/// Read one newline-framed message from a stdio peer.
///
/// Both hardenings below were in the full server loop and in neither the stub
/// nor anything else (CON-05):
///
/// 1. **Raw bytes, not `read_line`.** When the [`MAX_MESSAGE_SIZE`] `take`
///    boundary splits a multi-byte UTF-8 char, `read_line`'s UTF-8 validation
///    returns `Err(InvalidData)`; propagating that out of a long-lived session
///    loop kills the session over one oversized CJK request (H3). `read_until`
///    plus lossy decode tolerate it — malformed sequences become U+FFFD.
/// 2. **A size cap with a full drain.** An unterminated line cannot allocate
///    without bound, and the oversized line's tail is consumed through its
///    newline so it is not misparsed as the next message.
///
/// `buf` is caller-owned purely so the allocation can be reused across reads;
/// it is cleared on entry and its contents are not meaningful afterwards.
///
/// Callers keep their own blank-line skip and their own oversized-response
/// wording: this returns the framing decision, not the protocol reply.
pub fn read_frame<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>) -> std::io::Result<StdioFrame> {
    buf.clear();
    let n = reader
        .by_ref()
        .take(MAX_MESSAGE_SIZE as u64)
        .read_until(b'\n', buf)?;
    if n == 0 {
        return Ok(StdioFrame::Eof);
    }
    // Oversized: hit the `take` cap with no terminating newline. Checked on the
    // raw byte buffer before decoding, to avoid a huge lossy allocation.
    if buf.len() >= MAX_MESSAGE_SIZE && buf.last() != Some(&b'\n') {
        let oversized_len = buf.len();
        // Free the oversized buffer before draining, to avoid a 2x peak.
        buf.clear();
        buf.shrink_to(1024);
        // Drain until newline (line-aware), discarding the bytes. LOOP: a single
        // `take(MAX)` only consumes one MAX-sized chunk, so a line larger than
        // 2xMAX would leave a tail that gets misparsed as the next message. Keep
        // reading MAX-sized chunks until the terminating newline is consumed or
        // EOF is reached, so arbitrarily large lines are fully drained.
        let mut sink: Vec<u8> = Vec::new();
        loop {
            sink.clear();
            let drained = reader
                .by_ref()
                .take(MAX_MESSAGE_SIZE as u64)
                .read_until(b'\n', &mut sink)
                .unwrap_or(0);
            // EOF (nothing left) or we consumed through the newline → done.
            if drained == 0 || sink.last() == Some(&b'\n') {
                break;
            }
        }
        return Ok(StdioFrame::Oversized(oversized_len));
    }
    // Lossily decode: a `take`-truncated or otherwise malformed multi-byte
    // sequence becomes U+FFFD rather than killing the session (H3). Well-formed
    // JSON-RPC lines are unaffected.
    Ok(StdioFrame::Message(
        String::from_utf8_lossy(buf).into_owned(),
    ))
}
