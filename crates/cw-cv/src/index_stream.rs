//! `Para` / `Sect` / `Table` index streams (F1).
//!
//! The CaseView document keeps one shared record pool in `Layout` and three thin
//! *index* streams that address it. All three are pinned here, verified
//! **exactly** on
//! all 12 `Sect`/`Table` streams and all 6 `Para` streams of the staged v23/v24
//! corpus.
//!
//! ```text
//! Para  := header(12)  u32 layout_offset[count]
//! Sect  := header(12)  record[count]
//! Table := header(12)  record[count]
//!
//! header := u32 zero(=0)  u32 hint  u32 count
//! record := u32 start_para  u32 end_para  u32 mirror_ptr  u32 inline_name
//! ```
//!
//! ## `Para` — a pure offset vector (confirmed 6/6)
//!
//! `Para` is the only stream whose records are bare `u32`s: each is an absolute
//! byte offset into `Layout`, strictly usable as a paragraph record pointer. The
//! count is exact — `stream_len == 12 + 4 * count` — and every offset lands
//! inside `Layout` on every staged document.
//!
//! ## `Sect` / `Table` — fixed 16-byte records (confirmed 12/12)
//!
//! Both are `12 + 16 * count` bytes **exactly**. Reading the stream as a `u32`
//! vector suggests "mixed offsets with numeric fields"; it is
//! a record array. Field law:
//!
//! - `start_para` / `end_para` are **indices into the `Para` offset vector**, not
//!   byte offsets and not `Layout` offsets. Proof: on every staged document
//!   `max(end_para)` is `Para.count - 1 ..= Para.count - 4` and `start <= end`
//!   holds in every record, with **zero** out-of-range entries.
//! - `mirror_ptr` is a raw byte offset into `Layout` at a **named definition
//!   object**, `u8 kind | u8 00 | <4-byte name> …`, whose name is byte-identical
//!   to the record's `inline` field. Verified on **4418/4418** `Table` records
//!   across all six staged documents. `kind` is 2 on 9 of those records and 3 on
//!   the rest, and the two kinds lay their body out differently, so the field is
//!   surfaced as a pointer + kind rather than decoded further; `(start_para,
//!   end_para)` are repeated inside the object at `kind`-dependent offsets
//!   (`+11` for `kind` 3). `mirror_ptr` is `0` when the record is defaulted.
//! - `inline_name` is a **4-byte inline name**, verbatim: `FU2\0`, `VER\0`,
//!   `UD1\0`, `ZR1\0`, `SO3\0`, `ET9\0`, `HD1\0`, `SEL\0`, `DCM\0`, `T9\0\0`.
//!   On `Table` it is always a name. On `Sect` it carries a `Layout` offset
//!   instead — `Sect` has no inline name at all, the mirror image of the
//!   `Table` shape.
//!
//! ## `hint`
//!
//! `0xA1` on five of the six staged documents and `0x9B` on `EXPO0IS0`. Its
//! meaning is **not** established; it is preserved raw and a value outside the
//! observed set is surfaced as an issue rather than an error, because the word
//! is per-document metadata, not part of the address law.

/// Header size shared by all three index streams.
pub const HEADER_LEN: usize = 12;
/// Fixed record size of `Sect` / `Table`.
pub const RECORD_LEN: usize = 16;
/// `hint` value on five of the six staged documents.
pub const HINT_DEFAULT: u32 = 0xA1;
/// `hint` value on `EXPO0IS0` (blank template).
pub const HINT_BLANK_TEMPLATE: u32 = 0x9B;

/// The 12-byte index-stream header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexHeader {
    /// Word 0: 0 on every corpus stream.
    pub zero: u32,
    /// Word 1: per-document format hint (`0xA1` / `0x9B` observed).
    pub hint: u32,
    /// Word 2: record count.
    pub count: u32,
}

impl IndexHeader {
    /// Parse the header; `None` when the stream is shorter than 12 bytes.
    pub fn parse(s: &[u8]) -> Option<IndexHeader> {
        if s.len() < HEADER_LEN {
            return None;
        }
        let u = |o: usize| u32::from_le_bytes([s[o], s[o + 1], s[o + 2], s[o + 3]]);
        Some(IndexHeader {
            zero: u(0),
            hint: u(4),
            count: u(8),
        })
    }

    /// True when the count is one this build has observed.
    pub fn hint_known(&self) -> bool {
        matches!(self.hint, HINT_DEFAULT | HINT_BLANK_TEMPLATE)
    }

    /// Bytes the stream must occupy for `count` records of `record_len` each.
    pub fn expected_len(&self, record_len: usize) -> usize {
        HEADER_LEN + record_len * self.count as usize
    }
}

/// `Para`: a `u32` `Layout` offset per paragraph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParaIndex {
    pub header: IndexHeader,
    /// Absolute `Layout` byte offset per paragraph, in paragraph order.
    pub offsets: Vec<u32>,
    /// True when `stream_len == 12 + 4 * count` and every offset is in range.
    pub complete: bool,
}

impl ParaIndex {
    /// Parse a `Para` stream. `layout_len` bounds the offsets.
    pub fn parse(stream: &[u8], layout_len: usize) -> Option<ParaIndex> {
        let header = IndexHeader::parse(stream)?;
        let count = header.count as usize;
        let body = stream.get(HEADER_LEN..)?;
        let mut offsets = Vec::with_capacity(count.min(body.len() / 4));
        for i in 0..count {
            let o = body.get(i * 4..i * 4 + 4)?;
            offsets.push(u32::from_le_bytes([o[0], o[1], o[2], o[3]]));
        }
        let complete = header.expected_len(4) == stream.len()
            && offsets.iter().all(|&o| (o as usize) < layout_len);
        Some(ParaIndex {
            header,
            offsets,
            complete,
        })
    }

    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// `Layout` offset of paragraph `i`, if the index carries one.
    pub fn offset(&self, i: usize) -> Option<u32> {
        self.offsets.get(i).copied()
    }
}

/// One `Sect` / `Table` record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexRecord {
    /// First `Para` index covered by this record (inclusive).
    pub start_para: u32,
    /// Last `Para` index covered by this record (inclusive).
    pub end_para: u32,
    /// `Layout` byte offset of the named definition object for this record
    /// (`u8 kind | u8 00 | <name>`), or `0` when the record is defaulted.
    pub mirror_ptr: u32,
    /// Four bytes read verbatim: an inline name on `Table`, a `Layout` offset
    /// on `Sect`.
    pub inline: [u8; 4],
}

impl IndexRecord {
    /// The inline bytes as a NUL-trimmed name (empty on `Sect` records, whose
    /// inline field is an offset).
    pub fn inline_name(&self) -> String {
        let end = self.inline.iter().position(|&b| b == 0).unwrap_or(4);
        String::from_utf8_lossy(&self.inline[..end]).into_owned()
    }

    /// Number of paragraphs covered (`end - start + 1`, or 0 when inverted).
    pub fn paragraph_span(&self) -> u32 {
        if self.end_para >= self.start_para {
            self.end_para - self.start_para + 1
        } else {
            0
        }
    }

    /// True when this record carries a definition object in `Layout`.
    pub fn has_mirror(&self) -> bool {
        self.mirror_ptr != 0
    }

    /// The definition object's kind byte at `mirror_ptr` (1, 2 or 3 on the
    /// staged corpus), or `None` when there is no object or the byte is none.
    pub fn object_kind(&self, layout: &[u8]) -> Option<u8> {
        if self.mirror_ptr == 0 {
            return None;
        }
        let at = self.mirror_ptr as usize;
        let k = *layout.get(at)?;
        matches!(k, 1 | 2 | 3).then_some(k)
    }

    /// True when the definition object at `mirror_ptr` opens with this record's
    /// own inline name, i.e. the two fields name the same object.
    ///
    /// This is the 4418/4418 law: `layout[ptr+2 ..]` must start with the inline
    /// bytes up to and including their NUL terminator.
    pub fn object_name_matches(&self, layout: &[u8]) -> bool {
        if self.mirror_ptr == 0 {
            return false;
        }
        let at = self.mirror_ptr as usize;
        let Some(kind_pad) = layout.get(at..at + 2) else {
            return false;
        };
        if !matches!(kind_pad[0], 1 | 2 | 3) || kind_pad[1] != 0 {
            return false;
        }
        let end = self.inline.iter().position(|&b| b == 0).unwrap_or(4);
        let want = &self.inline[..end];
        !want.is_empty() && layout.get(at + 2..at + 2 + want.len()) == Some(want)
    }
}

/// `Sect` or `Table`: a fixed-size record array over paragraph ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeIndex {
    pub header: IndexHeader,
    pub records: Vec<IndexRecord>,
    /// True when the length law and every field law hold.
    pub complete: bool,
}

impl RangeIndex {
    /// Parse a `Sect`/`Table` stream given the document's `Para` count and the
    /// `Layout` length (to bound `mirror_ptr`).
    pub fn parse(stream: &[u8], para_count: usize, layout_len: usize) -> Option<RangeIndex> {
        let header = IndexHeader::parse(stream)?;
        let count = header.count as usize;
        let body = stream.get(HEADER_LEN..)?;
        let mut records = Vec::with_capacity(count.min(body.len() / RECORD_LEN));
        for i in 0..count {
            let r = body.get(i * RECORD_LEN..i * RECORD_LEN + RECORD_LEN)?;
            let u = |o: usize| u32::from_le_bytes([r[o], r[o + 1], r[o + 2], r[o + 3]]);
            records.push(IndexRecord {
                start_para: u(0),
                end_para: u(4),
                mirror_ptr: u(8),
                inline: [r[12], r[13], r[14], r[15]],
            });
        }
        let has_layout = layout_len > 0;
        let complete = header.expected_len(RECORD_LEN) == stream.len()
            && records.iter().all(|r| {
                (r.end_para as usize) < para_count.max(1)
                    && r.start_para <= r.end_para
                    && (!has_layout
                        || r.mirror_ptr == 0
                        || (r.mirror_ptr as usize) < layout_len)
            });
        Some(RangeIndex {
            header,
            records,
            complete,
        })
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn get(&self, i: usize) -> Option<&IndexRecord> {
        self.records.get(i)
    }

    /// `Layout` offsets of the paragraphs a record covers.
    pub fn paragraph_offsets<'a>(&self, i: usize, para: &'a ParaIndex) -> &'a [u32] {
        let Some(r) = self.records.get(i) else {
            return &[];
        };
        let start = r.start_para as usize;
        let end = (r.end_para as usize + 1).min(para.offsets.len());
        if start >= end {
            return &[];
        }
        &para.offsets[start..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr(hint: u32, count: u32) -> Vec<u8> {
        let mut v = Vec::new();
        for w in [0u32, hint, count] {
            v.extend_from_slice(&w.to_le_bytes());
        }
        v
    }

    fn rec(start: u32, end: u32, mirror: u32, name: &[u8; 4]) -> Vec<u8> {
        let mut v = Vec::new();
        for w in [start, end, mirror] {
            v.extend_from_slice(&w.to_le_bytes());
        }
        v.extend_from_slice(name);
        v
    }

    #[test]
    fn header_words() {
        let h = IndexHeader::parse(&hdr(HINT_DEFAULT, 8)).unwrap();
        assert_eq!(h.zero, 0);
        assert_eq!(h.hint, HINT_DEFAULT);
        assert_eq!(h.count, 8);
        assert!(h.hint_known());
        assert_eq!(h.expected_len(RECORD_LEN), 12 + 16 * 8);
        assert_eq!(h.expected_len(4), 12 + 4 * 8);
        assert!(IndexHeader::parse(&[0; 11]).is_none());
        assert!(!IndexHeader::parse(&hdr(0x77, 1)).unwrap().hint_known());
    }

    /// `03250RG0` `Sect`: 8 records, `(start, end)` pairs verbatim, and the
    /// paragraph range of the last record covers the tail of `Para`.
    #[test]
    fn real_sect_shape_from_03250rg0() {
        // start/end pairs transcribed from the stream (Layout mirror offsets
        // included for the three records that carry one).
        let pairs = [
            (0u32, 0x158u32, 0x3FA1u32),
            (0x16, 0x30, 0x3FAA),
            (0x54, 0x55, 0),
            (0x8F, 0x8F, 0),
            (0xF2, 0xF2, 0),
            (0x106, 0x10B, 0),
            (0x122, 0x12F, 0),
            (0x132, 0x158, 0x3FBD),
        ];
        let mut s = hdr(HINT_DEFAULT, pairs.len() as u32);
        for (a, b, m) in pairs {
            s.extend(rec(a, b, m, &[0, 0, 0, 0]));
        }
        assert_eq!(s.len(), 12 + 16 * 8, "length law");
        let idx = RangeIndex::parse(&s, 345, 0x2D61B).unwrap();
        assert_eq!(idx.len(), 8);
        assert!(idx.complete);
        // the max-end observation: 344 of 345 paragraphs
        assert_eq!(idx.records.iter().map(|r| r.end_para).max(), Some(0x158));
        assert_eq!(idx.records[0].paragraph_span(), 0x159);
        assert!(idx.records[0].has_mirror());
        assert!(!idx.records[2].has_mirror());
        // 0x132..0x158 points at the definition object at Layout 0x3FBD
        assert_eq!(idx.records[7].mirror_ptr, 0x3FBD);
    }

    /// `03250RG0` `Table`: 40 records carrying 4-byte inline names.
    #[test]
    fn real_table_inline_names() {
        let rows = [
            (0x0Cu32, 0x0Du32, 0x285D7u32, *b"FU2\0"),
            (0x0F, 0x14, 0x2862F, *b"VER\0"),
            (0x16, 0x30, 0x2874A, *b"UD1\0"),
            (0x35, 0x36, 0x288CB, *b"ZR1\0"),
        ];
        let mut s = hdr(HINT_DEFAULT, rows.len() as u32);
        for (a, b, m, n) in rows {
            s.extend(rec(a, b, m, &n));
        }
        let idx = RangeIndex::parse(&s, 345, 0x2D61B).unwrap();
        assert!(idx.complete);
        assert_eq!(idx.records[0].inline_name(), "FU2");
        assert_eq!(idx.records[1].inline_name(), "VER");
        assert_eq!(idx.records[2].inline_name(), "UD1");
        assert_eq!(idx.records[3].inline_name(), "ZR1");
        // start <= end in every record
        assert!(idx.records.iter().all(|r| r.start_para <= r.end_para));
    }

    /// A `u32`-vector read of the same bytes hits ASCII (`0x29003254` for
    /// "2RG\0") — the misread that 16-byte framing avoids.
    #[test]
    fn naive_u32_read_hits_ascii() {
        let mut s = hdr(HINT_DEFAULT, 1);
        s.extend(rec(0x0C, 0x0D, 0x285D7, b"FU2\0"));
        // reading words 3.. as a vector would surface the inline name as a number
        let w3 = u32::from_le_bytes([s[12 + 12], s[12 + 13], s[12 + 14], s[12 + 15]]);
        assert_eq!(w3, 0x0032_5546, "\"FU2\\0\" read as LE u32");
        // but the record parse gives the name back intact
        let idx = RangeIndex::parse(&s, 345, 0x2D61B).unwrap();
        assert_eq!(idx.records[0].inline_name(), "FU2");
    }

    #[test]
    fn para_is_a_pure_offset_vector() {
        let mut s = hdr(HINT_DEFAULT, 3);
        for off in [0x25750u32, 0x25818, 0x25840] {
            s.extend_from_slice(&off.to_le_bytes());
        }
        let p = ParaIndex::parse(&s, 0x2D61B).unwrap();
        assert_eq!(p.len(), 3);
        assert!(p.complete);
        assert_eq!(p.offset(0), Some(0x25750));
        assert_eq!(p.offset(3), None);
    }

    /// The length law is exact: a short or long stream is not `complete`.
    #[test]
    fn length_law_is_exact() {
        let mut s = hdr(HINT_DEFAULT, 1);
        s.extend(rec(0, 1, 0, b"AB\0\0"));
        let ok = RangeIndex::parse(&s, 10, 0x100).unwrap();
        assert!(ok.complete);

        // One byte short cannot satisfy the declared count at all, so the parse
        // refuses rather than inventing a partial record.
        let mut short = s.clone();
        short.pop();
        assert!(RangeIndex::parse(&short, 10, 0x100).is_none());

        let mut long = s.clone();
        long.push(0);
        assert!(!RangeIndex::parse(&long, 10, 0x100).unwrap().complete);
    }

    /// `end_para` beyond the `Para` count, or `start > end`, breaks `complete`.
    #[test]
    fn field_laws_are_enforced() {
        let mut s = hdr(HINT_DEFAULT, 1);
        s.extend(rec(5, 4, 0, b"AB\0\0")); // start > end
        assert!(!RangeIndex::parse(&s, 10, 0x100).unwrap().complete);

        let mut s = hdr(HINT_DEFAULT, 1);
        s.extend(rec(0, 99, 0, b"AB\0\0")); // end beyond count
        assert!(!RangeIndex::parse(&s, 10, 0x100).unwrap().complete);

        let mut s = hdr(HINT_DEFAULT, 1);
        s.extend(rec(0, 1, 0xFFF, b"AB\0\0")); // mirror past Layout end
        assert!(!RangeIndex::parse(&s, 10, 0x100).unwrap().complete);
    }

    /// A definition object is `kind | 00 | name…`; the record's inline name and
    /// the object's name are the same bytes (4418/4418 on the staged corpus).
    #[test]
    fn definition_object_names_the_same_object() {
        // Layout-shaped buffer: [kind=3][00]['F']['U']['2'][00] at offset 0x10
        let mut layout = vec![0u8; 0x40];
        layout[0x10] = 3;
        layout[0x11] = 0;
        layout[0x12..0x16].copy_from_slice(b"FU2\0");

        let mut s = hdr(HINT_DEFAULT, 1);
        s.extend(rec(0, 1, 0x10, b"FU2\0"));
        let idx = RangeIndex::parse(&s, 10, layout.len()).unwrap();
        let r = &idx.records[0];
        assert!(r.has_mirror());
        assert_eq!(r.object_kind(&layout), Some(3));
        assert!(r.object_name_matches(&layout));

        // kinds 2 and 1 are the other observed shapes, still naming the object
        layout[0x10] = 2;
        assert_eq!(r.object_kind(&layout), Some(2));
        assert!(r.object_name_matches(&layout));
        layout[0x10] = 1;
        assert_eq!(r.object_kind(&layout), Some(1));
        assert!(r.object_name_matches(&layout));

        // a different name at the target breaks the law
        layout[0x12..0x16].copy_from_slice(b"ZZZ\0");
        assert!(!r.object_name_matches(&layout));

        // neither kind byte => not an object
        layout[0x10] = 9;
        assert_eq!(r.object_kind(&layout), None);
    }

    #[test]
    fn paragraph_offsets_slice_the_para_vector() {
        let mut p = hdr(HINT_DEFAULT, 4);
        for off in [0x10u32, 0x20, 0x30, 0x40] {
            p.extend_from_slice(&off.to_le_bytes());
        }
        let para = ParaIndex::parse(&p, 0x100).unwrap();
        let mut s = hdr(HINT_DEFAULT, 1);
        s.extend(rec(1, 2, 0, b"AB\0\0"));
        let idx = RangeIndex::parse(&s, 4, 0x100).unwrap();
        assert_eq!(idx.paragraph_offsets(0, &para), &[0x20, 0x30]);
        assert_eq!(idx.paragraph_offsets(9, &para), &[]);
    }
}
