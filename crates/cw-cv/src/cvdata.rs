//! `CvData` stream: the document identity block (F1).
//!
//! Corpus shape (byte-exact, 193/193 files that carry a non-empty CvData):
//!
//! ```text
//! CvData := header(20) count(u16) "FORM\0" <form-code>\0 "VERSION\0" …
//!           u32 offsets  "<<CvData_Valid>>"
//! ```
//!
//! `header` bytes 0…19 are `01 00 00 00 00 00 01 00 …`, `count` is 1. The form
//! code (`01200RG`, `02720GG`, `04000RG`, …) equals the worksheet/registry code
//! carried in the `.cvw` filename and is the join key for the linked-worksheet
//! registry (F2). The four trailing u32s are offsets inside the tag block (their
//! meaning is not needed yet; preserved raw).

#[derive(Debug, Clone, Default)]
pub struct CvData {
    /// Form code following `FORM\0` (e.g. `01200RG`).
    pub form: Option<String>,
    /// Token following `VERSION\0`, if any.
    pub version: Option<String>,
    /// True when the `<<CvData_Valid>>` end marker is present.
    pub valid: bool,
    /// Number of tag offsets preceding the end marker.
    pub offset_count: usize,
}

impl CvData {
    pub fn parse(data: &[u8]) -> CvData {
        let form = token_after(data, b"FORM\0");
        let version = token_after(data, b"VERSION\0");
        let valid = find(data, b"<<CvData_Valid>>").is_some();
        let offset_count = find(data, b"<<CvData_Valid>>")
            .map(|end| count_u32_tail(&data[..end]))
            .unwrap_or(0);
        CvData {
            form,
            version,
            valid,
            offset_count,
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn token_after(data: &[u8], tag: &[u8]) -> Option<String> {
    let at = find(data, tag)? + tag.len();
    let rest = &data[at..];
    let end = rest.iter().position(|&b| b == 0)?;
    if end == 0 {
        return None;
    }
    let token = &rest[..end];
    // `VERSION` is followed directly by the u32 tag offsets: the "token" is a
    // control byte, not text. Only accept printable ASCII as a token.
    if token.iter().any(|&b| !(0x20..0x7F).contains(&b)) {
        return None;
    }
    Some(token.iter().map(|&b| b as char).collect())
}

/// Count trailing little-endian u32s that look like ascending small offsets.
fn count_u32_tail(data: &[u8]) -> usize {
    let mut n = 0;
    let mut pos = data.len();
    while pos >= 4 {
        let v = u32::from_le_bytes([data[pos - 4], data[pos - 3], data[pos - 2], data[pos - 1]]);
        if v == 0 || v > data.len() as u32 * 8 {
            break;
        }
        n += 1;
        pos -= 4;
        if n > 64 {
            break;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_corpus_cvdata() {
        let data = hex(CVDATA);
        let cv = CvData::parse(&data);
        assert_eq!(cv.form.as_deref(), Some("01200RG"));
        assert!(cv.valid);
        assert!(cv.version.is_none());
        assert_eq!(cv.offset_count, 4);
    }

    #[test]
    fn missing_tags_are_none() {
        let cv = CvData::parse(b"");
        assert!(cv.form.is_none());
        assert!(!cv.valid);
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// 77-byte CvData of `2024 Hull Lift Truck Review(1148441)01200RG0.cvw`.
    const CVDATA: &str = "\
010000000000010000000000000000000000000000000100464f524d00303132303052470056455253494f4e\
00180000001d000000250000002d0000003c3c4376446174615f56616c69643e3e";
}

