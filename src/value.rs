//! Decoding raw Redis string payloads for human display.
//!
//! Shopware/Symfony cache values are very often compressed (the Symfony cache
//! `DeflateMarshaller`, or Shopware's zstd marshaller), so a plain `GET` returns
//! binary. We sniff the leading magic bytes, decompress when recognized, and
//! report what we did so the UI can show e.g. "decompressed from zstd".

use std::io::Read;

/// What compression (if any) a payload turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Plain,
    Gzip,
    Zlib,
    Zstd,
}

impl Encoding {
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Plain => "plain",
            Encoding::Gzip => "gzip",
            Encoding::Zlib => "zlib/deflate",
            Encoding::Zstd => "zstd",
        }
    }
}

/// A decoded payload ready for display. Carries *both* a text and a hex
/// rendering so the UI can toggle between them without re-fetching.
#[derive(Debug, Clone)]
pub struct Decoded {
    /// How the original bytes were compressed (Plain if not / undecodable).
    pub encoding: Encoding,
    /// Byte offset the compressed stream started at. `Some(n)` with `n > 0`
    /// means it was *embedded* after a wrapper (e.g. a Symfony cache envelope),
    /// not the whole value.
    pub stream_offset: Option<usize>,
    /// Text rendering: the exact string when valid UTF-8, otherwise a lossy
    /// render (invalid bytes become `�`) so readable structure still shows.
    pub text: String,
    /// Offset/hex/ascii dump rendering of the bytes.
    pub hex: String,
    /// True when the (decompressed) bytes are not valid UTF-8 — the UI defaults
    /// to the hex view in that case.
    pub is_binary: bool,
}

/// Detect compression from the leading magic bytes.
fn sniff(bytes: &[u8]) -> Encoding {
    match bytes {
        // gzip: 1f 8b
        [0x1f, 0x8b, ..] => Encoding::Gzip,
        // zstd: 28 b5 2f fd
        [0x28, 0xb5, 0x2f, 0xfd, ..] => Encoding::Zstd,
        // zlib: 78 with a valid FCHECK (01, 9c, da are the common levels)
        [0x78, 0x01 | 0x5e | 0x9c | 0xda, ..] => Encoding::Zlib,
        _ => Encoding::Plain,
    }
}

fn inflate_gzip(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut out)
        .ok()?;
    Some(out)
}

fn inflate_zlib(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(bytes)
        .read_to_end(&mut out)
        .ok()?;
    Some(out)
}

fn inflate_zstd(bytes: &[u8]) -> Option<Vec<u8>> {
    // Decode a *single* frame and stop. `decode_all` (and a plain streaming
    // read) treat any bytes after the frame as the start of a second frame and
    // fail with "Unknown frame descriptor" — which is exactly the case for an
    // embedded stream (Symfony stores `s:<len>:"<zstd frame>"`, so the closing
    // quote and serialized tail follow the frame).
    let mut out = Vec::new();
    zstd::stream::read::Decoder::new(bytes)
        .ok()?
        .single_frame()
        .read_to_end(&mut out)
        .ok()?;
    Some(out)
}

/// Try to decompress `bytes` as `enc`, returning the inflated payload. We
/// require a non-trivial result so a coincidental magic-byte match on random
/// data doesn't get mistaken for a real stream.
fn try_decompress(bytes: &[u8], enc: Encoding) -> Option<Vec<u8>> {
    let out = match enc {
        Encoding::Gzip => inflate_gzip(bytes),
        Encoding::Zlib => inflate_zlib(bytes),
        Encoding::Zstd => inflate_zstd(bytes),
        Encoding::Plain => None,
    }?;
    (out.len() >= 8).then_some(out)
}

/// Scan `raw` for the first embedded compression stream that actually inflates.
/// Returns (encoding, offset, inflated). Used for Symfony cache wrappers where a
/// binary header + serialized envelope precedes the real (compressed) value.
fn find_embedded(raw: &[u8]) -> Option<(Encoding, usize, Vec<u8>)> {
    // Limit how far in we look so a huge value can't cost too much.
    let scan_end = raw.len().min(8192);
    for i in 0..scan_end {
        let window = &raw[i..];
        let enc = match window {
            [0x1f, 0x8b, ..] => Encoding::Gzip,
            [0x28, 0xb5, 0x2f, 0xfd, ..] => Encoding::Zstd,
            [0x78, 0x01 | 0x5e | 0x9c | 0xda, ..] => Encoding::Zlib,
            _ => continue,
        };
        if let Some(out) = try_decompress(window, enc) {
            return Some((enc, i, out));
        }
    }
    None
}

/// Decode a raw Redis string value: decompress when recognized, then render as
/// UTF-8 text or a hex dump. `max_text` bounds the rendered text length so a
/// huge value can't stall the UI.
///
/// Handles three cases, in order:
///  1. the whole value is a compression stream (leading magic),
///  2. a compression stream is *embedded* after a wrapper (Symfony cache
///     envelopes prepend a binary header + serialized tag list),
///  3. plain / undecodable bytes (shown as text or hex).
pub fn decode(raw: &[u8], max_text: usize) -> Decoded {
    let (encoding, stream_offset, bytes) = match sniff(raw) {
        Encoding::Plain => {
            // Not compressed from the front — look for an embedded stream.
            match find_embedded(raw) {
                Some((enc, off, out)) => (enc, Some(off), out),
                None => (Encoding::Plain, None, raw.to_vec()),
            }
        }
        enc => match try_decompress(raw, enc) {
            Some(out) => (enc, Some(0), out),
            None => (Encoding::Plain, None, raw.to_vec()),
        },
    };

    let is_binary = std::str::from_utf8(&bytes).is_err();
    // Text view uses a lossy render so the readable parts (e.g. PHP-serialized
    // structure) still come through even when some bytes are invalid UTF-8.
    let text = truncate_chars(&String::from_utf8_lossy(&bytes), max_text);
    let hex = hex_dump(&bytes, max_text);
    Decoded {
        encoding,
        stream_offset,
        text,
        hex,
        is_binary,
    }
}

/// Truncate to `max` characters on a char boundary, appending a note.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    let remaining = s.chars().count() - max;
    format!("{head}\n\n… {remaining} more characters (truncated)")
}

/// Render the first `max` bytes as a classic offset/hex/ascii dump.
fn hex_dump(bytes: &[u8], max: usize) -> String {
    let shown = bytes.len().min(max);
    let mut out = String::new();
    for (i, chunk) in bytes[..shown].chunks(16).enumerate() {
        let offset = i * 16;
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|&b| {
                if (0x20..0x7f).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        out.push_str(&format!("{offset:08x}  {:<48}  {ascii}\n", hex.join(" ")));
    }
    if bytes.len() > shown {
        out.push_str(&format!(
            "… {} more bytes (truncated)\n",
            bytes.len() - shown
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }
    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn plain_utf8_passes_through() {
        let d = decode(b"hello world", 1000);
        assert_eq!(d.encoding, Encoding::Plain);
        assert_eq!(d.text, "hello world");
        assert!(!d.is_binary);
    }

    #[test]
    fn detects_and_inflates_gzip() {
        let raw = gzip(b"the quick brown fox");
        assert_eq!(sniff(&raw), Encoding::Gzip);
        let d = decode(&raw, 1000);
        assert_eq!(d.encoding, Encoding::Gzip);
        assert_eq!(d.text, "the quick brown fox");
    }

    #[test]
    fn detects_and_inflates_zlib() {
        let raw = zlib(b"deflate me please");
        assert_eq!(sniff(&raw), Encoding::Zlib);
        let d = decode(&raw, 1000);
        assert_eq!(d.encoding, Encoding::Zlib);
        assert_eq!(d.text, "deflate me please");
    }

    #[test]
    fn finds_zlib_stream_embedded_after_a_wrapper() {
        // Mimics a Symfony cache envelope: binary header + serialized tag list,
        // then the inner value as a zlib stream.
        let inner = b"a:2:{s:4:\"core\";a:1:{s:5:\"email\";s:5:\"a@b.c\";}}";
        let mut raw = vec![0x9d, 0, 0, 0, 0, 0, 0, 0, 0];
        raw.extend_from_slice(b"a:1:{i:0;s:13:\"system-config\";}s:2082:\"");
        let off = raw.len();
        raw.extend(zlib(inner));

        let d = decode(&raw, 10_000);
        assert_eq!(d.encoding, Encoding::Zlib);
        assert_eq!(d.stream_offset, Some(off), "wrong embedded offset");
        assert!(
            d.text.contains("a:2:") && d.text.contains("email"),
            "inner not decoded"
        );
        assert!(!d.is_binary, "decoded inner should be text");
    }

    #[test]
    fn leading_stream_reports_offset_zero() {
        let raw = zlib(b"plain stream");
        let d = decode(&raw, 1000);
        assert_eq!(d.stream_offset, Some(0));
    }

    #[test]
    fn binary_without_any_stream_has_no_offset() {
        let d = decode(&[0x9d, 0x00, 0x01, 0x02, 0xff, 0xfe], 1000);
        assert_eq!(d.encoding, Encoding::Plain);
        assert_eq!(d.stream_offset, None);
        assert!(d.is_binary);
    }

    #[test]
    fn detects_and_inflates_zstd() {
        let raw = zstd::stream::encode_all(&b"zstd payload here"[..], 0).unwrap();
        assert_eq!(sniff(&raw), Encoding::Zstd);
        let d = decode(&raw, 1000);
        assert_eq!(d.encoding, Encoding::Zstd);
        assert_eq!(d.text, "zstd payload here");
    }

    #[test]
    fn finds_embedded_zstd_frame_with_trailing_bytes() {
        // Regression: Symfony stores the inner value as `s:<len>:"<zstd frame>"`,
        // so a closing quote and serialized tail follow the frame. A whole-input
        // zstd decode rejects those trailing bytes ("Unknown frame descriptor");
        // we must decode just the first frame.
        let inner = b"a:2:{i:0;s:9:\"some-type\";}payload-body-here";
        let frame = zstd::stream::encode_all(&inner[..], 0).unwrap();

        let mut raw = vec![0x9d, 0, 0, 0, 0, 0, 0, 0, 0];
        raw.extend_from_slice(b"a:1:{i:0;s:9:\"some-type\";}s:99:\"");
        let off = raw.len();
        raw.extend_from_slice(&frame);
        raw.extend_from_slice(b"\";"); // trailing bytes after the frame

        let d = decode(&raw, 10_000);
        assert_eq!(d.encoding, Encoding::Zstd);
        assert_eq!(d.stream_offset, Some(off));
        assert!(
            d.text.contains("some-type") && d.text.contains("payload-body-here"),
            "inner not decoded: {}",
            d.text
        );
    }

    #[test]
    fn non_utf8_is_flagged_binary_with_both_views() {
        let d = decode(&[0x00, 0xff, 0xfe, 0x01], 1000);
        assert!(d.is_binary);
        // hex view always carries the dump…
        assert!(d.hex.contains("00 ff fe 01"));
        // …and the text view is a lossy render (replacement chars), not a dump.
        assert!(!d.text.contains("00 ff fe 01"));
    }

    #[test]
    fn utf8_value_still_provides_a_hex_view() {
        let d = decode(b"hi", 1000);
        assert!(!d.is_binary);
        assert_eq!(d.text, "hi");
        // hex is always available so the UI can toggle even for text values.
        assert!(d.hex.contains("68 69")); // 'h' 'i'
    }

    #[test]
    fn long_text_is_truncated() {
        let s = "a".repeat(5000);
        let d = decode(s.as_bytes(), 100);
        assert!(d.text.contains("truncated"));
    }

    #[test]
    fn corrupt_gzip_magic_falls_back_to_plain() {
        // Looks like gzip (1f 8b) but isn't valid -> we keep raw, no panic.
        let d = decode(&[0x1f, 0x8b, 0x00, 0x00], 1000);
        assert_eq!(d.encoding, Encoding::Plain);
    }
}
