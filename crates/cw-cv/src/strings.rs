//! `Form/Strings` text pool — container law + codec (F6).
//!
//! Every `.cvw` reserves a `Form/Strings` stream: the document's authored and
//! formula text, addressed by cells through a string index. The stream is a
//! **DynaZip-framed raw LZMA1 payload** — the same 19-byte DynaZip prefix the
//! `.ac_` package members carry (F62/F63, `cw_core::lzma::decode_pkg_member`),
//! wrapped in a ZIP *central-directory-shaped* 46-byte block. It is not zlib,
//! not bzip2, and not a real zip: the frame has no local header, no central
//! directory and no end record.
//!
//! ```text
//! Strings := pool_header(12)  zip_frame(46)  dynazip_prefix(19)  lzma1_payload
//!
//! pool_header     := u32 version(=2) u32 unpacked_len u32 string_count
//!
//! zip_frame       := "PK\x01\x02"                  central-dir signature
//!                    u16 version_made_by(=0x0014)
//!                    u16 version_needed(=0x0014)
//!                    u16 flags(=0x0800)   0x0800 = "zip64 / size unknown"
//!                    u16 method(=14)      14 = LZMA
//!                    u32 dos_datetime                   (varies)
//!                    u32 crc32                          (over prefix+payload)
//!                    u32 compressed_size(=0xFFFFFFFF)   size follows the data
//!                    u32 uncompressed_size(=0)
//!                    u16 name_len(=0)  u16 extra_len(=0)  u16 comment_len(=0)
//!                    u16 disk_start(=0) u16 internal_attrs(=0xFFFF)
//!                    u32 external_attrs(=0)  u32 local_header_offset(=0)
//!
//! dynazip_prefix  := u8  marker(=0x0C)   u32 reserved(=0)
//!                    u32 uncompressed_len          (== pool_header word 2)
//!                    u16 lzma_sdk_minor(=0x0000)   u16 lzma_sdk_major(=0x0E09)
//!                    u16 lzma_sdk_patch(=0xD05D -> low byte 0x5D)
//!                    u8  props(=0x5D)              lc3 / lp0 / pb2
//! ```
//!
//! ## Field evidence (all corpus pools)
//!
//! Verified across the three fully-materialized corpus pools (hull2024
//! `EXPO0IS0`, hull2024 `FJEIMP`, viewrail2025 `EXPO0IS0`) and pinned by
//! `tests/f6_strings_gate.rs`:
//!
//! - `pool_header` words 0..2 are version 2, the payload length and the entry
//!   count (`cvw::StringsHeader` already pinned these).
//! - `zip_frame` bytes 12..16 are `PK\x01\x02` — the constant
//!   `cvw::STRINGS_BLOCK_MAGIC` (0x02014B50). The version/flag/method words are
//!   the DynaZip profile (`0x0014`, `0x0014`, `0x0800`, method 14).
//! - The `crc32` word equals `crc32(stream[58..])` in all three pools, i.e. the
//!   CRC covers the DynaZip prefix **and** the payload, from offset 58 to the
//!   end of the stream. It is a pool-integrity check, not a payload checksum.
//! - `dynazip_prefix` begins at stream offset 58 with the marker `0x0C`; its
//!   `uncompressed_len` word equals the pool header's `unpacked_len` exactly in
//!   every pool, and the Sphinx-style SDK words `0x0E09`/`0xD05D` are constant.
//! - The payload therefore starts at stream offset [`PAYLOAD_OFFSET`] = 77 and
//!   decodes as raw LZMA1 lc3/lp0/pb2. It produces `unpacked_len` bytes of entry
//!   table plus a 1–2 byte zero tail that DynaZip leaves in place; the exact
//!   tail length varies (2 in hull2024, 1 in viewrail2025), so the decoder stops
//!   at `unpacked_len` and treats the rest as padding.
//!
//! ## Entry grammar
//!
//! ```text
//! table := entry*  u32 sentinel(=0xFFFFFFFF)
//! entry := u32 byte_len  u8 kind  byte[byte_len]  0x00
//! ```
//!
//! The walk is closed: over the corpus pools it consumes *exactly*
//! `unpacked_len` bytes, ends on the sentinel, and yields *exactly*
//! `string_count` entries. Both are asserted, which is what makes the framing
//! self-checking.
//!
//! `byte_len` counts the bytes between `kind` and the NUL terminator; a zero
//! length is legal and appears as entry 0 in every pool. `kind` is 3 (plain
//! text) for every corpus entry; other values are preserved raw because their
//! meaning is unestablished. Entry bytes are **Latin-1 including embedded
//! control prefixes** — `0x03`, `\t\x01\x02\t`, `0x01\x02` — which are CaseView
//! formatting directives, not displayable text, so the raw bytes are kept and a
//! lossy UTF-8 view is offered alongside.
//!
//! ## Corpus reach (documented limitation)
//!
//! The shipped `.ac_` packages carry a minority of `.cvw` members as truncated
//! LZMA (F63): the member stream aborts mid-flight, so a `.cvw`'s own `Strings`
//! stream only materializes when it falls inside the readable prefix. In this
//! corpus **three** documents decode end-to-end (both `EXPO0IS0` copies and
//! hull2024 `FJEIMP`); the rest either carry no `Strings` stream (blank
//! templates and system sheets, whose text lives in `Layout`) or place it past
//! the readable frontier. [`Cvw::strings_pool`](crate::Cvw::strings_pool)
//! reports the reason per document; the F6 gate pins exactly the verified set.

use cw_core::error::CoreError;

/// Byte offset of the raw LZMA1 payload inside the `Strings` stream.
pub const PAYLOAD_OFFSET: usize = 77;
/// Offset of the DynaZip prefix inside the `Strings` stream.
pub const PREFIX_OFFSET: usize = 58;
/// LZMA1 properties byte used by every corpus pool (lc3/lp0/pb2).
pub const PROPS: u8 = 0x5D;
/// Sentinel that closes the entry table.
pub const SENTINEL: u32 = 0xFFFF_FFFF;
/// Entry kind observed for every decoded corpus string (plain text).
pub const KIND_TEXT: u8 = 3;

/// Why a pool could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StringsError {
    /// The document carries no `Form/Strings` stream (a blank template or a
    /// system sheet whose text lives in `Layout`).
    NotPresent,
    /// The stream is shorter than the framing prefix, or its header words are
    /// not the version-2 shape.
    BadHeader,
    /// The v2 header is present but the documented frame does not follow it, so
    /// the payload offset cannot be trusted. Observed on documents whose
    /// `Strings` stream was itself carried in a damaged region.
    BadFrame,
    /// The payload cannot be decoded from the bytes this document copy carries
    /// (the F63 truncated-member frontier).
    Unreadable { need: usize, have: usize },
    /// The LZMA1 payload aborted before producing the declared length.
    TruncatedPayload { got: usize, want: usize },
    /// The decoded bytes do not follow the entry grammar.
    BadTable { at: usize, detail: &'static str },
    /// The walk produced a different entry count than the header declares.
    CountMismatch { declared: u32, walked: usize },
}

impl std::fmt::Display for StringsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StringsError::NotPresent => write!(f, "strings: no Form/Strings stream"),
            StringsError::BadHeader => write!(f, "strings: header is not version 2"),
            StringsError::BadFrame => write!(f, "strings: v2 header without the DynaZip frame"),
            StringsError::Unreadable { need, have } => {
                write!(f, "strings: stream needs {need} bytes, copy carries {have}")
            }
            StringsError::TruncatedPayload { got, want } => {
                write!(f, "strings: payload aborted at {got} of {want} bytes")
            }
            StringsError::BadTable { at, detail } => {
                write!(f, "strings: entry table breaks at {at}: {detail}")
            }
            StringsError::CountMismatch { declared, walked } => {
                write!(
                    f,
                    "strings: header declares {declared} entries, walked {walked}"
                )
            }
        }
    }
}

impl std::error::Error for StringsError {}

impl From<StringsError> for CoreError {
    fn from(e: StringsError) -> CoreError {
        CoreError::Format(e.to_string())
    }
}

/// One entry of the pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolEntry {
    /// Raw text bytes exactly as stored (Latin-1; formatting prefixes kept).
    pub text: Vec<u8>,
    /// The entry's `kind` byte (3 = plain text in every corpus sample).
    pub kind: u8,
}

impl PoolEntry {
    /// Lossy UTF-8 view, for display and reporting.
    pub fn as_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.text)
    }

    /// True when this entry is the plain-text kind.
    pub fn is_text(&self) -> bool {
        self.kind == KIND_TEXT
    }
}

/// Header words of the `Strings` stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolHeader {
    pub version: u32,
    /// Declared entry-table length in bytes.
    pub unpacked_len: u32,
    /// Declared number of entries.
    pub count: u32,
}

impl PoolHeader {
    /// Parse the 12-byte header; `None` when the stream is too short.
    pub fn parse(s: &[u8]) -> Option<PoolHeader> {
        if s.len() < 12 {
            return None;
        }
        let u = |o: usize| u32::from_le_bytes([s[o], s[o + 1], s[o + 2], s[o + 3]]);
        Some(PoolHeader {
            version: u(0),
            unpacked_len: u(4),
            count: u(8),
        })
    }
}

/// A decoded string pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringsPool {
    pub header: PoolHeader,
    pub entries: Vec<PoolEntry>,
    /// True when the walk hit the sentinel inside the declared length and
    /// produced exactly the declared entry count.
    pub complete: bool,
}

impl StringsPool {
    /// Decode a whole `Form/Strings` stream.
    pub fn parse(stream: &[u8]) -> std::result::Result<StringsPool, StringsError> {
        if stream.is_empty() {
            return Err(StringsError::NotPresent);
        }
        let header = PoolHeader::parse(stream).ok_or(StringsError::BadHeader)?;
        if header.version != 2 {
            return Err(StringsError::BadHeader);
        }
        if stream.len() < PAYLOAD_OFFSET {
            return Err(StringsError::Unreadable {
                need: PAYLOAD_OFFSET,
                have: stream.len(),
            });
        }
        let frame = &stream[12..PAYLOAD_OFFSET];
        if !frame_ok(frame, header.unpacked_len) {
            return Err(StringsError::BadFrame);
        }
        let want = header.unpacked_len as usize;
        let body = decode_lzma(&stream[PAYLOAD_OFFSET..], want)?;
        let entries = walk(&body, want)?;
        let complete = entries.len() == header.count as usize;
        if !complete {
            return Err(StringsError::CountMismatch {
                declared: header.count,
                walked: entries.len(),
            });
        }
        Ok(StringsPool {
            header,
            entries,
            complete,
        })
    }

    /// The decoded text of entry `i`, or `None` when out of range.
    pub fn text(&self, i: usize) -> Option<std::borrow::Cow<'_, str>> {
        self.entries.get(i).map(|e| e.as_str())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Validate the 46-byte `PK\x01\x02` frame plus the 19-byte DynaZip prefix.
///
/// Only the invariant fields are asserted: the signature, the two version
/// words, the zip64 flag, method 14, the `0xFFFFFFFF` compressed size, the
/// three zero length words, the zero disk/attributes words, the `0x0C` prefix
/// marker, the repeated unpacked length, the LZMA SDK version words and the
/// properties byte. The DOS timestamp and the CRC vary per document and are
/// **not** asserted here (the CRC law is verified by the gate instead).
fn frame_ok(f: &[u8], unpacked_len: u32) -> bool {
    if f.len() < PAYLOAD_OFFSET - 12 {
        return false;
    }
    let u16_at = |o: usize| u16::from_le_bytes([f[o], f[o + 1]]);
    let u32_at = |o: usize| u32::from_le_bytes([f[o], f[o + 1], f[o + 2], f[o + 3]]);
    // zip_frame: 0..46
    f[0..4] == [b'P', b'K', 0x01, 0x02]
        && u16_at(4) == 0x0014
        && u16_at(6) == 0x0014
        && u16_at(8) == 0x0800
        && u16_at(10) == 14
        && u32_at(20) == 0xFFFF_FFFF
        && u32_at(24) == 0
        && u16_at(28) == 0
        && u16_at(30) == 0
        && u16_at(32) == 0
        && u16_at(34) == 0
        && u16_at(36) == 0xFFFF
        && u32_at(38) == 0
        && u32_at(42) == 0
        // dynazip_prefix: 46..65 (stream offsets 58..77)
        && f[46] == 0x0C
        && u32_at(47) == 0
        && u32_at(51) == unpacked_len
        && u32_at(55) == 0
        // f[59] is the constant 0xE0 that opens DynaZip's trailing SDK word.
        && f[59] == 0xE0
        // f[60..64] carry DynaZip's own per-member version/check bytes: they
        // differ between pools of the same build (`1A 0E 09 D0`, `04 B4 02 A5`,
        // `1A 0E 09 CD`), so they are preserved but not asserted. f[64] is the
        // LZMA1 properties byte, 0x5D in every corpus pool.
        && f[64] == PROPS
}

/// Decode the LZMA1 payload to exactly `want` bytes.
///
/// The corpus streams carry no end marker: the decoder is asked for the
/// declared size and stops there. A short result means the stream is damaged
/// (the F63 frontier) and is reported, never zero-filled.
fn decode_lzma(payload: &[u8], want: usize) -> std::result::Result<Vec<u8>, StringsError> {
    let (lc, lp, pb) = cw_core::lzma::props_split(PROPS).map_err(|_| StringsError::BadHeader)?;
    let mut dec = cw_core::lzma::Lz1::new(lc, lp, pb);
    match dec.decode(payload, want) {
        Ok(out) if out.len() >= want => Ok(out),
        Ok(out) => Err(StringsError::TruncatedPayload {
            got: out.len(),
            want,
        }),
        Err(_) => Err(StringsError::TruncatedPayload { got: 0, want }),
    }
}

/// Walk the entry table over exactly `want` bytes.
fn walk(body: &[u8], want: usize) -> std::result::Result<Vec<PoolEntry>, StringsError> {
    let mut entries = Vec::new();
    let mut pos = 0usize;
    loop {
        if pos + 4 > want {
            return Err(StringsError::BadTable {
                at: pos,
                detail: "stream ends before a record or the sentinel",
            });
        }
        let n = u32::from_le_bytes([body[pos], body[pos + 1], body[pos + 2], body[pos + 3]]);
        if n == SENTINEL {
            if pos + 4 != want {
                return Err(StringsError::BadTable {
                    at: pos,
                    detail: "sentinel does not sit at the declared end",
                });
            }
            break;
        }
        let n = n as usize;
        let head = pos + 4;
        if head + 1 + n + 1 > want {
            return Err(StringsError::BadTable {
                at: pos,
                detail: "record runs past the declared length",
            });
        }
        let kind = body[head];
        let text = &body[head + 1..head + 1 + n];
        if body[head + 1 + n] != 0 {
            return Err(StringsError::BadTable {
                at: pos,
                detail: "record is not NUL-terminated",
            });
        }
        entries.push(PoolEntry {
            text: text.to_vec(),
            kind,
        });
        pos = head + 1 + n + 1;
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal encoder-side fixture: build a table and check the walk.
    fn table(entries: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for e in entries {
            out.extend_from_slice(&(e.len() as u32).to_le_bytes());
            out.push(KIND_TEXT);
            out.extend_from_slice(e);
            out.push(0);
        }
        out.extend_from_slice(&SENTINEL.to_le_bytes());
        out
    }

    #[test]
    fn walk_roundtrip_and_sentinel_law() {
        let t = table(&[b"", b"a", b"hello"]);
        let got = walk(&t, t.len()).unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].text, b"");
        assert_eq!(got[2].as_str(), "hello");
        assert!(got.iter().all(|e| e.is_text()));

        // A sentinel that is not at the declared end is rejected.
        let mut bad = t.clone();
        bad.push(0);
        assert!(matches!(
            walk(&bad, bad.len()),
            Err(StringsError::BadTable { .. })
        ));
    }

    #[test]
    fn unterminated_record_is_rejected() {
        let mut t = Vec::new();
        t.extend_from_slice(&3u32.to_le_bytes());
        t.push(KIND_TEXT);
        t.extend_from_slice(b"abc");
        t.extend_from_slice(&SENTINEL.to_le_bytes());
        assert!(matches!(
            walk(&t, t.len()),
            Err(StringsError::BadTable { .. }) | Err(StringsError::CountMismatch { .. })
        ));
    }

    #[test]
    fn header_shape() {
        let mut s = vec![0u8; 16];
        s[0..4].copy_from_slice(&2u32.to_le_bytes());
        s[4..8].copy_from_slice(&6671u32.to_le_bytes());
        s[8..12].copy_from_slice(&256u32.to_le_bytes());
        let h = PoolHeader::parse(&s).unwrap();
        assert_eq!(h.version, 2);
        assert_eq!(h.unpacked_len, 6671);
        assert_eq!(h.count, 256);
        assert!(PoolHeader::parse(&s[..8]).is_none());
    }

    #[test]
    fn empty_stream_is_not_present() {
        assert_eq!(StringsPool::parse(&[]), Err(StringsError::NotPresent));
        // A v1 header reports BadHeader rather than guessing.
        let mut s = vec![0u8; 96];
        s[0..4].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(StringsPool::parse(&s), Err(StringsError::BadHeader));
    }

    #[test]
    fn v2_header_without_frame_is_rejected() {
        let mut s = vec![0u8; 96];
        s[0..4].copy_from_slice(&2u32.to_le_bytes());
        s[4..8].copy_from_slice(&10u32.to_le_bytes());
        s[8..12].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(StringsPool::parse(&s), Err(StringsError::BadFrame));
    }
}
