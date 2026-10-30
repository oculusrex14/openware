//! dBase III table reader/writer — the CaseWare working-table store.
//!
//! Covers every version byte observed in the corpus (F25, golden freeze
//! 2026-10-22): `0x03` plain dBase III, `0x30` Fox variant, `0xF5` the
//! CaseWare-marked variant (record data plaintext — F27).
//!
//! Byte-stability contract (P5.C1): `encode(parse(bytes)) == bytes` for every
//! golden table. The raw header block (descriptors + terminator + any pad,
//! e.g. TM's 264 pad bytes) and raw record cells are carried verbatim; the
//! writer re-emits `header ++ records ++ 0x1A`.

use crate::error::{CoreError, Result};

/// dBase field descriptor.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    /// ASCII type letter: C N D L M (corpus census F25 — no F/B/I in the wild).
    pub kind: u8,
    pub len: u8,
    pub dec: u8,
}

/// One physical record: flag byte + concatenated cell bytes (`record_len` total).
pub type RawRecord = Vec<u8>;

#[derive(Debug, Clone)]
pub struct Table {
    /// Verbatim header bytes `[0..header_len)`: disk header, descriptors,
    /// `0x0D` terminator, any trailing pad.
    pub raw_header: Vec<u8>,
    pub version: u8,
    pub header_len: u16,
    pub record_len: u16,
    pub fields: Vec<Field>,
    /// Physical records in file order, deletion flags preserved.
    pub records: Vec<RawRecord>,
}

/// Byte offset of cell `i` inside a record (flag occupies 0).
fn cell_offsets(fields: &[Field]) -> Vec<usize> {
    let mut offs = Vec::with_capacity(fields.len());
    let mut o = 1usize;
    for f in fields {
        offs.push(o);
        o += f.len as usize;
    }
    offs
}

impl Table {
    pub fn parse(bytes: &[u8]) -> Result<Table> {
        if bytes.len() < 33 {
            return Err(CoreError::Format(format!("too small: {} bytes", bytes.len())));
        }
        let version = bytes[0];
        let count = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let header_len = u16::from_le_bytes([bytes[8], bytes[9]]);
        let record_len = u16::from_le_bytes([bytes[10], bytes[11]]);
        if (header_len as usize) > bytes.len() {
            return Err(CoreError::Format(format!(
                "header_len {header_len} > file {}",
                bytes.len()
            )));
        }
        // descriptor walk
        let mut fields = Vec::new();
        let mut off = 32usize;
        loop {
            if off >= header_len as usize {
                return Err(CoreError::Format("no field terminator 0x0D in header".into()));
            }
            if bytes[off] == 0x0D {
                off += 1;
                break;
            }
            if off + 32 > header_len as usize {
                return Err(CoreError::Format("truncated field descriptor".into()));
            }
            let d = &bytes[off..off + 32];
            let raw_name = &d[0..11];
            let name_end = raw_name.iter().position(|&b| b == 0).unwrap_or(11);
            fields.push(Field {
                name: String::from_utf8_lossy(&raw_name[..name_end]).into_owned(),
                kind: d[11],
                len: d[16],
                dec: d[17],
            });
            off += 32;
        }
        if fields.is_empty() {
            return Err(CoreError::Format("no fields".into()));
        }
        if off != header_len as usize {
            // post-terminator pad (TM family): fine, carried via raw_header.
        }
        let sum: usize = 1 + fields.iter().map(|f| f.len as usize).sum::<usize>();
        if sum != record_len as usize {
            return Err(CoreError::Format(format!(
                "field lens +1 = {sum} != record_len {record_len}"
            )));
        }
        let expected = header_len as usize + count as usize * record_len as usize + 1;
        if bytes.len() != expected {
            return Err(CoreError::Format(format!(
                "size {} != header {} + {} x rec {} + 1",
                bytes.len(),
                header_len,
                count,
                record_len
            )));
        }
        if bytes[expected - 1] != 0x1A {
            return Err(CoreError::Format("missing EOF 0x1A".into()));
        }
        let mut records = Vec::with_capacity(count as usize);
        let mut p = header_len as usize;
        for _ in 0..count {
            records.push(bytes[p..p + record_len as usize].to_vec());
            p += record_len as usize;
        }
        Ok(Table {
            raw_header: bytes[..header_len as usize].to_vec(),
            version,
            header_len,
            record_len,
            fields,
            records,
        })
    }

    pub fn parse_path(path: impl AsRef<std::path::Path>) -> Result<Table> {
        let bytes = std::fs::read(path)?;
        Table::parse(&bytes)
    }

    /// Byte-exact serialization (the no-op save path).
    ///
    /// The on-disk record count (header bytes 4..8) is refreshed from
    /// `records` before emission; for any parsed-then-unmodified table this
    /// is already consistent, so C1 byte-stability is preserved (F58). Needed
    /// by record-set rewriting writers (P5.D1 copy engine, F64).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            self.header_len as usize + self.records.len() * self.record_len as usize + 1,
        );
        out.extend_from_slice(&self.raw_header);
        let n = self.records.len() as u32;
        if out.len() >= 8 && u32::from_le_bytes([out[4], out[5], out[6], out[7]]) != n {
            out[4..8].copy_from_slice(&n.to_le_bytes());
        }
        for r in &self.records {
            debug_assert_eq!(r.len(), self.record_len as usize, "record length drift");
            out.extend_from_slice(r);
        }
        out.push(0x1A);
        out
    }

    pub fn write_path(&self, path: impl AsRef<std::path::Path>) -> Result<()> {
        std::fs::write(path, self.encode())?;
        Ok(())
    }

    pub fn count(&self) -> usize {
        self.records.len()
    }

    pub fn field_index(&self, name: &str) -> Option<usize> {
        let name_l = name.to_ascii_uppercase();
        self.fields
            .iter()
            .position(|f| f.name.to_ascii_uppercase() == name_l)
    }

    pub fn offsets(&self) -> Vec<usize> {
        cell_offsets(&self.fields)
    }

    /// Raw cell bytes for record `r`, field `name`.
    pub fn cell(&self, r: usize, name: &str) -> Option<&[u8]> {
        let idx = self.field_index(name)?;
        let rec = self.records.get(r)?;
        let off = self.offsets();
        let start = off[idx];
        Some(&rec[start..start + self.fields[idx].len as usize])
    }

    pub fn is_deleted(&self, r: usize) -> bool {
        self.records.get(r).map(|r| r[0] == 0x2A).unwrap_or(false)
    }

    /// Latin-1 decoded cell (CW text is ANSI/codepage; never UTF-8 in corpus).
    pub fn cell_str(&self, r: usize, name: &str) -> Option<String> {
        crate::canon::latin1(self.cell(r, name)?).into()
    }
}

/// Synthetic two-field table (C5 + N4.2, one deleted row, TM-style header pad).
/// Test fixture shared by unit tests and `healthy()`; NOT corpus data.
pub fn synthetic_fixture() -> Vec<u8> {
    let mut h = Vec::new();
    h.push(0xF5);
    h.extend_from_slice(&[0x7A, 0x0A, 0x16]); // date yy mm dd
    h.extend_from_slice(&3u32.to_le_bytes()); // count
    let header_len: usize = 32 + 2 * 32 + 1 + 7; // +7 pad, the TM pattern
    h.extend_from_slice(&(header_len as u16).to_le_bytes());
    let record_len: usize = 1 + 5 + 4;
    h.extend_from_slice(&(record_len as u16).to_le_bytes());
    h.extend(std::iter::repeat(0u8).take(20)); // reserved[12..32], dBase header
    let mut f1 = [0u8; 32];
    f1[..4].copy_from_slice(b"CODE");
    f1[11] = b'C';
    f1[16] = 5;
    let mut f2 = [0u8; 32];
    f2[..3].copy_from_slice(b"AMT");
    f2[11] = b'N';
    f2[16] = 4;
    f2[17] = 2;
    h.extend_from_slice(&f1);
    h.extend_from_slice(&f2);
    h.push(0x0D);
    h.extend(std::iter::repeat(0u8).take(7));
    let rec = |code: &str, amt: &str, flag: u8| {
        let mut r = Vec::new();
        r.push(flag);
        let mut c = code.as_bytes().to_vec();
        c.resize(5, b' ');
        r.extend(c);
        let mut n = amt.as_bytes().to_vec();
        n.resize(4, b' ');
        r.extend(n);
        r
    };
    h.extend(rec("A0001", "1000", 0x20));
    h.extend(rec("A0002", "-2.5", 0x2A)); // deleted
    h.extend(rec("A0003", "0.03", 0x20));
    h.push(0x1A);
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_synthetic() {
        let bytes = synthetic_fixture();
        let t = Table::parse(&bytes).unwrap();
        assert_eq!(t.count(), 3);
        assert_eq!(t.fields.len(), 2);
        assert_eq!(t.fields[1].name, "AMT");
        assert_eq!(t.field_index("amt"), Some(1));
        assert!(t.is_deleted(1));
        assert_eq!(t.cell_str(0, "CODE").as_deref(), Some("A0001"));
        assert_eq!(t.cell(0, "AMT").unwrap(), b"1000");
        assert_eq!(t.cell(1, "AMT").unwrap(), b"-2.5");
        assert_eq!(t.encode(), bytes);
    }

    #[test]
    fn rejects_truncated() {
        let mut bytes = synthetic_fixture();
        bytes.pop();
        assert!(Table::parse(&bytes).is_err());
    }

    #[test]
    fn rejects_record_len_mismatch() {
        let mut bytes = synthetic_fixture();
        bytes[10] = 0; // record_len -> 1, fields sum to 10
        assert!(Table::parse(&bytes).is_err());
    }

    #[test]
    fn encode_refreshes_record_count() {
        let mut t = Table::parse(&synthetic_fixture()).unwrap();
        t.records.truncate(1);
        let bytes = t.encode();
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 1);
        let re = Table::parse(&bytes).unwrap();
        assert_eq!(re.count(), 1);
        // ...and stays byte-stable on an already-consistent table
        assert_eq!(re.encode(), bytes);
    }
}

