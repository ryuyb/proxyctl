//! Decoding the kernel's streaming responses.
//!
//! # Why this cannot be folded into the existing response parser
//!
//! [`super::unix::parse`](super::unix) reads a complete body and splits it at the
//! header terminator. The observation endpoints do not have a complete body: they
//! are `Transfer-Encoding: chunked` and they never end. Measured against the
//! kernel, all four of `/traffic`, `/memory`, `/connections`, and `/logs` hold the
//! connection open until the client gives up.
//!
//! So there are two distinct jobs — "read a document" and "read an endless
//! sequence of documents" — and they get two distinct implementations rather than
//! one with a mode flag.
//!
//! # Chunked framing
//!
//! Each chunk is a hexadecimal size line, then that many bytes, then CRLF. A
//! zero-length chunk ends the body. The kernel sends one JSON document per chunk,
//! but that is an observation, not a guarantee: this decoder reassembles a
//! **byte stream** and leaves line splitting to the caller, so a document split
//! across two chunks still arrives intact.

use proxy_application::ports::PortError;

/// The largest header block accepted, in bytes.
///
/// A response header is small. The bound exists so a peer that sends an endless
/// stream of header-looking bytes cannot make this grow without limit.
const MAX_HEADER_BYTES: usize = 64 * 1024;

/// A parsed response head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    /// The HTTP status.
    pub status: u16,
    /// Whether the body uses chunked framing.
    pub chunked: bool,
    /// Bytes of body that arrived alongside the head.
    pub body: Vec<u8>,
}

/// Splits a byte buffer into a response head and the body bytes that follow.
///
/// Returns [`None`] when the head is not complete yet, so a caller reading a
/// socket incrementally can ask again after more bytes arrive.
///
/// # Errors
///
/// Returns [`PortError::InvalidResponse`] when the head is malformed or exceeds
/// [`MAX_HEADER_BYTES`].
pub fn split_head(buffer: &[u8]) -> Result<Option<Head>, PortError> {
    let Some(position) = find_head_end(buffer) else {
        if buffer.len() > MAX_HEADER_BYTES {
            return Err(PortError::InvalidResponse(
                "the response header never terminated".to_owned(),
            ));
        }
        return Ok(None);
    };

    let head = String::from_utf8_lossy(&buffer[..position]).into_owned();
    let rest = buffer[position..].to_vec();

    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| PortError::InvalidResponse(format!("no status code in {status_line:?}")))?;

    // Case-insensitive, because header names are.
    let chunked = lines.any(|line| {
        let Some((name, value)) = line.split_once(':') else {
            return false;
        };
        name.trim().eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().contains("chunked")
    });

    Ok(Some(Head {
        status,
        chunked,
        body: rest,
    }))
}

/// Finds the offset just past the header terminator.
fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|start| start + 4)
}

/// A chunked-body decoder that tolerates partial input.
///
/// Fed arbitrary slices and asked for whatever complete chunks they contain. The
/// incremental shape is the point: a socket read returns whatever has arrived, and
/// a decoder that demanded a whole chunk per call would drop data whenever a chunk
/// straddled two reads.
#[derive(Debug, Default)]
pub struct ChunkedDecoder {
    /// Bytes received but not yet consumed.
    pending: Vec<u8>,
    /// Whether the terminating zero-length chunk has been seen.
    finished: bool,
}

impl ChunkedDecoder {
    /// Creates a decoder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the terminating chunk has been seen.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        self.finished
    }

    /// Adds received bytes.
    pub fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
    }

    /// Removes and returns the next complete chunk's payload.
    ///
    /// Returns [`None`] when no complete chunk has arrived yet. A chunk that is
    /// still incomplete stays buffered.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::InvalidResponse`] when a size line is not hexadecimal,
    /// or when the framing is otherwise unusable.
    pub fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, PortError> {
        if self.finished {
            return Ok(None);
        }

        let Some(line_end) = self.pending.windows(2).position(|w| w == b"\r\n") else {
            return Ok(None);
        };

        let size_line = String::from_utf8_lossy(&self.pending[..line_end]).into_owned();
        // A chunk extension (`1a;name=value`) is legal and ignored.
        let size_text = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| {
            PortError::InvalidResponse(format!("chunk size {size_text:?} is not hexadecimal"))
        })?;

        let after_size = line_end + 2;
        // A zero-length chunk is the terminator, optionally followed by trailers
        // and a final CRLF. Nothing after it is body.
        if size == 0 {
            self.finished = true;
            self.pending.clear();
            return Ok(None);
        }

        // Wait for the payload *and* its trailing CRLF.
        let needed = after_size + size + 2;
        if self.pending.len() < needed {
            return Ok(None);
        }

        let chunk = self.pending[after_size..after_size + size].to_vec();
        self.pending.drain(..needed);

        // Look ahead for the terminator. Without this, `is_finished` stays false
        // after the last data chunk until the caller asks for one more — and a
        // caller that stops reading once it has what it wants would never learn
        // the stream had ended, leaving it to discover that from a timeout.
        if let Some(end) = self.pending.windows(2).position(|w| w == b"\r\n") {
            let size_text = String::from_utf8_lossy(&self.pending[..end]).into_owned();
            let size_text = size_text.split(';').next().unwrap_or("").trim();
            if size_text == "0" {
                self.finished = true;
                self.pending.clear();
            }
        }

        Ok(Some(chunk))
    }

    /// Whether any undecoded bytes remain buffered.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(text: &str) -> Head {
        split_head(text.as_bytes())
            .expect("must parse")
            .expect("must be complete")
    }

    #[test]
    fn a_plain_response_reports_its_status_and_body() {
        let parsed = head("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"a\":1}");
        assert_eq!(parsed.status, 200);
        assert!(!parsed.chunked);
        assert_eq!(parsed.body, b"{\"a\":1}");
    }

    /// The kernel really sends this, so the header must be recognised.
    #[test]
    fn a_chunked_response_is_recognised() {
        let parsed = head(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
             Transfer-Encoding: chunked\r\n\r\n",
        );
        assert_eq!(parsed.status, 200);
        assert!(parsed.chunked, "chunked framing must be detected");
    }

    /// Header names are case-insensitive, and the value may carry parameters.
    #[test]
    fn chunked_detection_is_case_insensitive() {
        for value in [
            "transfer-encoding: chunked",
            "TRANSFER-ENCODING: CHUNKED",
            "Transfer-Encoding: chunked, gzip",
        ] {
            let text = format!("HTTP/1.1 200 OK\r\n{value}\r\n\r\n");
            let parsed = head(&text);
            assert!(parsed.chunked, "{value}");
        }
    }

    /// An incomplete head is not an error: the caller reads more and retries.
    #[test]
    fn an_incomplete_head_is_not_an_error() {
        assert_eq!(split_head(b"HTTP/1.1 200 OK\r\n").expect("no error"), None);
        assert_eq!(split_head(b"").expect("no error"), None);
    }

    /// An endless run of header bytes must not grow the buffer forever.
    #[test]
    fn an_unterminated_head_is_bounded() {
        let big = vec![b'x'; MAX_HEADER_BYTES + 10];
        assert!(split_head(&big).is_err());
    }

    #[test]
    fn a_head_without_a_status_code_is_refused() {
        assert!(split_head(b"GARBAGE\r\n\r\n").is_err());
    }

    /// One chunk, delivered whole.
    #[test]
    fn a_single_chunk_decodes() {
        let mut decoder = ChunkedDecoder::new();
        decoder.push(b"5\r\nhello\r\n0\r\n\r\n");
        assert_eq!(decoder.next_chunk().expect("ok"), Some(b"hello".to_vec()));
        assert!(decoder.is_finished());
    }

    /// Several chunks arrive one at a time.
    #[test]
    fn several_chunks_decode_in_order() {
        let mut decoder = ChunkedDecoder::new();
        decoder.push(b"3\r\nfoo\r\n3\r\nbar\r\n0\r\n\r\n");
        assert_eq!(decoder.next_chunk().expect("ok"), Some(b"foo".to_vec()));
        assert_eq!(decoder.next_chunk().expect("ok"), Some(b"bar".to_vec()));
        assert_eq!(decoder.next_chunk().expect("ok"), None);
        assert!(decoder.is_finished());
    }

    /// A chunk straddling two reads must not lose data. This is the case a
    /// whole-chunk-per-read decoder silently corrupts.
    #[test]
    fn a_chunk_split_across_reads_is_reassembled() {
        let mut decoder = ChunkedDecoder::new();
        decoder.push(b"8\r\nabcd");
        assert_eq!(
            decoder.next_chunk().expect("no error yet"),
            None,
            "an incomplete chunk must not be reported"
        );
        decoder.push(b"efgh\r\n");
        assert_eq!(
            decoder.next_chunk().expect("ok"),
            Some(b"abcdefgh".to_vec())
        );
    }

    /// A split landing inside the size line must also work.
    #[test]
    fn a_split_size_line_is_tolerated() {
        let mut decoder = ChunkedDecoder::new();
        decoder.push(b"1");
        assert_eq!(decoder.next_chunk().expect("ok"), None);
        decoder.push(b"0\r\n0123456789abcdef\r\n");
        assert_eq!(
            decoder.next_chunk().expect("ok"),
            Some(b"0123456789abcdef".to_vec())
        );
    }

    /// Chunk extensions are legal and must be ignored rather than parsed as part
    /// of the size.
    #[test]
    fn a_chunk_extension_is_ignored() {
        let mut decoder = ChunkedDecoder::new();
        decoder.push(b"5;name=value\r\nhello\r\n");
        assert_eq!(decoder.next_chunk().expect("ok"), Some(b"hello".to_vec()));
    }

    /// A JSON document larger than one read arrives across several chunks, and
    /// the caller concatenates them. The decoder must not assume one doc per chunk.
    #[test]
    fn a_document_spanning_chunks_is_delivered_piecewise() {
        let mut decoder = ChunkedDecoder::new();
        // `{"a":1}\n` split as `{"a"` + `:1}\n`.
        decoder.push(b"4\r\n{\"a\"\r\n4\r\n:1}\n\r\n0\r\n\r\n");
        let first = decoder.next_chunk().expect("ok").expect("a chunk");
        let second = decoder.next_chunk().expect("ok").expect("a chunk");
        let mut joined = first;
        joined.extend_from_slice(&second);
        assert_eq!(joined, b"{\"a\":1}\n");
    }

    #[test]
    fn a_non_hexadecimal_size_is_refused() {
        let mut decoder = ChunkedDecoder::new();
        decoder.push(b"zz\r\nhello\r\n");
        assert!(decoder.next_chunk().is_err());
    }

    /// Once finished, further calls return nothing rather than looping on
    /// leftover bytes.
    #[test]
    fn a_finished_decoder_stays_finished() {
        let mut decoder = ChunkedDecoder::new();
        decoder.push(b"0\r\n\r\n");
        assert_eq!(decoder.next_chunk().expect("ok"), None);
        assert!(decoder.is_finished());
        assert_eq!(decoder.next_chunk().expect("ok"), None);
    }

    /// The real kernel sends one JSON object per chunk; this asserts the shape
    /// against captured bytes so the decoder is exercised on real framing.
    #[test]
    fn real_traffic_framing_decodes() {
        // Sizes are measured, not guessed: `{"up":0,"down":0,"upTotal":2715}\n` is
        // 33 bytes (0x21) and the second is 36 (0x24). Getting these wrong is how
        // a hand-written framing fixture becomes a decoder bug report.
        let raw = b"21\r\n{\"up\":0,\"down\":0,\"upTotal\":2715}\n\r\n24\r\n{\"up\":75,\"down\":870,\"upTotal\":2790}\n\r\n0\r\n\r\n";
        let mut decoder = ChunkedDecoder::new();
        decoder.push(raw);
        let mut documents = Vec::new();
        while let Some(chunk) = decoder.next_chunk().expect("ok") {
            documents.push(String::from_utf8_lossy(&chunk).into_owned());
        }
        assert_eq!(documents.len(), 2);
        assert!(documents[0].contains("\"upTotal\":2715"), "{documents:?}");
        assert!(documents[1].contains("\"upTotal\":2790"), "{documents:?}");
    }
}
