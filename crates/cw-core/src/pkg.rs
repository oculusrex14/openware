//! The `.ac_` engagement container (F20/F22/F24): a CFB whose
//! `CaseWareDocument` stream is a ZIP of the working set, plus metadata
//! side streams. This module owns container semantics + the zip codec
//! (reader: central+local parse, raw carry, stored decode; writer: raw-carry
//! or deflate-stored emit).
//!
//! Corpus compression census (F21): method 8 deflate x1 (the tiny
//! `<client>.ac` hub), method 14 LZMA xN — Caseware's LZMA streams are
//! LZMA-alone frames (5-byte props + 8-byte size, dict sometimes 0) hidden in
//! the zip field; their decode is a tracked follow-up
//! (`docs/specs/pkg-container.md` §7). The write path never re-compresses:
//! unchanged entries are carried verbatim, new ones stored/deflate-stored —
//! both decodable by every zip reader, Caseware's included.

use crate::cfb::{Cfb, CfbBuilder};
use crate::error::{CoreError, Result};

pub const S_DOCUMENT: &str = "CaseWareDocument";
pub const S_COMPRESSION: &str = "CompressionVersion";
pub const S_FILEINFO: &str = "FileInformation";
pub const S_TEMPLATE: &str = "TemplateInfo";
pub const S_PROTECTION: &str = "CaseWareProtectionInfo";
pub const S_USERLIST: &str = "CaseWareUserlist";

/// Canonical stream set of a Caseware package (observed; 2 pkg corpora).
pub const CANONICAL_STREAMS: [&str; 10] = [
    "\u{5}DocumentSummaryInformation",
    "\u{5}SummaryInformation",
    S_DOCUMENT,
    S_PROTECTION,
    S_USERLIST,
    S_COMPRESSION,
    "DmsProperties",
    "DmsVersionInfo",
    S_FILEINFO,
    S_TEMPLATE,
];

/// Expected `CompressionVersion` payload prefix (F20).
pub const DYNZIP: &str = "DynaZip";

// ---------------------------------------------------------------------------
// crc32 (IEEE, reflected)
// ---------------------------------------------------------------------------

pub fn crc32(data: &[u8]) -> u32 {
    fn table() -> &'static [u32; 256] {
        use std::sync::OnceLock;
        static T: OnceLock<[u32; 256]> = OnceLock::new();
        T.get_or_init(|| {
            let mut t = [0u32; 256];
            let mut i = 0usize;
            while i < 256 {
                let mut c = i as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
                }
                t[i] = c;
                i += 1;
            }
            t
        })
    }
    let t = table();
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = t[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

// ---------------------------------------------------------------------------
// zip
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ZipEntry {
    pub name: String,
    pub method: u16,
    pub flags: u16,
    pub crc: u32,
    pub compressed_len: usize,
    pub len: usize,
    /// Offset of the local header within the zip byte stream.
    pub local_ofs: usize,
}

#[derive(Debug, Clone)]
pub enum ZipSource {
    /// Existing compressed payload, carried verbatim (the no-op path).
    Raw {
        method: u16,
        crc: u32,
        compressed: Vec<u8>,
        len: usize,
    },
    /// Fresh bytes, emitted deflate-stored (method 8).
    Stored(Vec<u8>),
}

#[derive(Default)]
pub struct ZipWriter {
    entries: Vec<(String, ZipSource)>,
}

impl ZipWriter {
    pub fn new() -> ZipWriter {
        ZipWriter::default()
    }

    pub fn add(&mut self, name: impl Into<String>, src: ZipSource) -> &mut Self {
        self.entries.push((name.into(), src));
        self
    }

    pub fn add_stored(&mut self, name: impl Into<String>, data: Vec<u8>) -> &mut Self {
        self.add(name, ZipSource::Stored(data))
    }

    /// Deflate with nothing but stored blocks: legal, universal, deterministic.
    pub fn deflate_stored(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + data.len() / 65_535 * 5 + 16);
        if data.is_empty() {
            out.extend_from_slice(&[0x01, 0x00, 0x00, 0xFF, 0xFF]);
            return out;
        }
        let mut off = 0usize;
        loop {
            let n = (data.len() - off).min(65_535);
            let last = off + n == data.len();
            out.push(if last { 0x01 } else { 0x00 }); // BFINAL, BTYPE=00
            out.extend_from_slice(&(n as u16).to_le_bytes());
            out.extend_from_slice(&(!(n as u16)).to_le_bytes());
            out.extend_from_slice(&data[off..off + n]);
            off += n;
            if last {
                break;
            }
        }
        out
    }

    /// Assemble local records + central directory + EOCD. Field values mirror
    /// the corpus (version 2.0, flags 0x0800 UTF-8, no extras, FAT epoch).
    pub fn build(self) -> Result<Vec<u8>> {
        struct Rec {
            name: Vec<u8>,
            method: u16,
            crc: u32,
            csize: u32,
            usize_: u32,
            local_ofs: u32,
        }
        let mut out: Vec<u8> = Vec::new();
        let mut recs: Vec<Rec> = Vec::with_capacity(self.entries.len());
        for (name, src) in self.entries {
            let nb = name.into_bytes();
            let (method, crc, csize, usize_, body) = match src {
                ZipSource::Raw { method, crc, compressed, len } => {
                    (method, crc, compressed.len() as u32, len as u32, compressed)
                }
                ZipSource::Stored(data) => {
                    let crc = crc32(&data);
                    let body = Self::deflate_stored(&data);
                    (8u16, crc, body.len() as u32, data.len() as u32, body)
                }
            };
            let local_ofs = out.len() as u32;
            let mut lh = Vec::with_capacity(30 + nb.len() + body.len());
            lh.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04]);
            lh.extend_from_slice(&20u16.to_le_bytes());
            lh.extend_from_slice(&0x0800u16.to_le_bytes());
            lh.extend_from_slice(&method.to_le_bytes());
            lh.extend_from_slice(&0u16.to_le_bytes()); // time
            lh.extend_from_slice(&0x0021u16.to_le_bytes()); // 1980-01-01
            lh.extend_from_slice(&crc.to_le_bytes());
            lh.extend_from_slice(&csize.to_le_bytes());
            lh.extend_from_slice(&usize_.to_le_bytes());
            lh.extend_from_slice(&(nb.len() as u16).to_le_bytes());
            lh.extend_from_slice(&0u16.to_le_bytes()); // extra
            lh.extend_from_slice(&nb);
            lh.extend_from_slice(&body);
            out.extend_from_slice(&lh);
            recs.push(Rec { name: nb, method, crc, csize, usize_, local_ofs });
        }
        let cd_ofs = out.len() as u32;
        let cd0 = out.len();
        for r in &recs {
            out.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02]);
            out.extend_from_slice(&20u16.to_le_bytes()); // made by (FAT)
            out.extend_from_slice(&20u16.to_le_bytes()); // needed
            out.extend_from_slice(&0x0800u16.to_le_bytes());
            out.extend_from_slice(&r.method.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // time
            out.extend_from_slice(&0x0021u16.to_le_bytes());
            out.extend_from_slice(&r.crc.to_le_bytes());
            out.extend_from_slice(&r.csize.to_le_bytes());
            out.extend_from_slice(&r.usize_.to_le_bytes());
            out.extend_from_slice(&(r.name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra
            out.extend_from_slice(&0u16.to_le_bytes()); // comment
            out.extend_from_slice(&0u16.to_le_bytes()); // disk start
            out.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            out.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            out.extend_from_slice(&r.local_ofs.to_le_bytes());
            let name = r.name.clone();
            out.extend_from_slice(&name);
        }
        let cd_size = (out.len() - cd0) as u32;
        let count = recs.len() as u16;
        out.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06]); // EOCD
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_ofs.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
        Ok(out)
    }
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Parsed zip (central directory + raw access into the source buffer).
pub struct Zip {
    pub entries: Vec<ZipEntry>,
}

impl Zip {
    pub fn parse(buf: &[u8]) -> Result<Zip> {
        let mut eocd = None;
        let floor = buf.len().saturating_sub(65_536 + 22);
        let mut i = buf.len();
        while i > floor {
            i -= 1;
            if buf[i] == 0x50
                && buf.get(i + 1) == Some(&0x4B)
                && buf.get(i + 2) == Some(&0x05)
                && buf.get(i + 3) == Some(&0x06)
            {
                eocd = Some(i);
                break;
            }
        }
        let e = eocd.ok_or_else(|| CoreError::Format("zip: no EOCD".into()))?;
        let count = u16le(buf, e + 10) as usize;
        let cd_size = u32le(buf, e + 12) as usize;
        let cd_ofs = u32le(buf, e + 16) as usize;
        if cd_ofs + cd_size > buf.len() {
            return Err(CoreError::Format("zip: central dir out of range".into()));
        }
        let mut entries = Vec::with_capacity(count);
        let mut p = cd_ofs;
        for _ in 0..count {
            if buf.get(p..p + 4) != Some(&[0x50, 0x4B, 0x01, 0x02][..]) {
                return Err(CoreError::Format(format!("zip: bad central @ {p}")));
            }
            let flags = u16le(buf, p + 8);
            let method = u16le(buf, p + 10);
            let crc = u32le(buf, p + 16);
            let csize = u32le(buf, p + 20) as usize;
            let usize_ = u32le(buf, p + 24) as usize;
            let nlen = u16le(buf, p + 28) as usize;
            let elen = u16le(buf, p + 30) as usize;
            let clen = u16le(buf, p + 32) as usize;
            let lho = u32le(buf, p + 42) as usize;
            let name_b = buf
                .get(p + 46..p + 46 + nlen)
                .ok_or_else(|| CoreError::Format("zip: name out of range".into()))?;
            let name = std::str::from_utf8(name_b)
                .map_err(|_| CoreError::Format("zip: name not utf8".into()))?
                .to_string();
            entries.push(ZipEntry {
                name,
                method,
                flags,
                crc,
                compressed_len: csize,
                len: usize_,
                local_ofs: lho,
            });
            p += 46 + nlen + elen + clen;
        }
        if entries.len() != count {
            return Err(CoreError::Format("zip: count mismatch".into()));
        }
        Ok(Zip { entries })
    }

    /// Compressed bytes of entry `i`, read via its LOCAL header (the Iter-22
    /// lesson: local name/extra lengths, never the central's).
    pub fn raw(&self, buf: &[u8], i: usize) -> Result<Vec<u8>> {
        let e = self
            .entries
            .get(i)
            .ok_or_else(|| CoreError::Format("zip: no such entry".into()))?;
        let p = e.local_ofs;
        if buf.get(p..p + 4) != Some(&[0x50, 0x4B, 0x03, 0x04][..]) {
            return Err(CoreError::Format("zip: bad local header".into()));
        }
        let nl = u16le(buf, p + 26) as usize;
        let el = u16le(buf, p + 28) as usize;
        let from = p + 30 + nl + el;
        let to = from.checked_add(e.compressed_len).ok_or_else(|| {
            CoreError::Format("zip: size overflow".into())
        })?;
        buf.get(from..to)
            .map(|x| x.to_vec())
            .ok_or_else(|| CoreError::Format("zip: data out of range".into()))
    }

    /// Method 0 payload decode.
    /// Decode entry `i` to its uncompressed bytes: method 0 verbatim, 8 raw
    /// deflate ([`crate::inflate`]), 14 the DynaZip LZMA frame
    /// ([`crate::lzma::decode_pkg_member`]). Strict: size and CRC must match.
    pub fn decode(&self, buf: &[u8], i: usize) -> Result<Vec<u8>> {
        let (out, crc_ok) = self.decode_lenient(buf, i)?;
        if !crc_ok {
            return Err(CoreError::Format(format!(
                "zip: {} crc mismatch",
                self.entries[i].name
            )));
        }
        Ok(out)
    }

    /// Best-effort decode: returns the payload and whether its CRC matched.
    /// CaseWare's own uncompress writes members whose stored CRC is bad (the
    /// corpus carries such members, F63); restore parity needs the same bytes.
    pub fn decode_lenient(&self, buf: &[u8], i: usize) -> Result<(Vec<u8>, bool)> {
        let e = self
            .entries
            .get(i)
            .ok_or_else(|| CoreError::Format("zip: entry index out of range".into()))?;
        let raw = self.raw(buf, i)?;
        let out = match e.method {
            0 => raw,
            8 => crate::inflate::inflate(&raw, e.len)?,
            14 => {
                if e.len == 0 {
                    Vec::new()
                } else {
                    crate::lzma::decode_pkg_member_truncating(&raw, e.len)?
                }
            }
            m => return Err(CoreError::Format(format!("zip: unsupported method {m}"))),
        };
        if e.len > 0 && out.len() != e.len {
            return Err(CoreError::Format(format!(
                "zip: {} decoded {} bytes, header says {}",
                e.name,
                out.len(),
                e.len
            )));
        }
        let crc_ok = crc32(&out) == e.crc;
        Ok((out, crc_ok))
    }

    pub fn decode_stored(data: &[u8]) -> Vec<u8> {
        data.to_vec()
    }
}

// ---------------------------------------------------------------------------
// container
// ---------------------------------------------------------------------------

pub struct Pkg {
    pub cfb: Cfb,
}

/// Engagement name per `FileInformation` (plaintext, NUL/CRLF-terminated).
pub fn engagement_name(file_information: &[u8]) -> String {
    let s = String::from_utf8_lossy(file_information);
    s.trim_end_matches(['\0', '\r', '\n']).trim().to_string()
}

impl Pkg {
    pub fn parse(buf: &[u8]) -> Result<Pkg> {
        Ok(Pkg { cfb: Cfb::parse(buf)? })
    }

    pub fn stream(&self, name: &str) -> Result<Vec<u8>> {
        self.cfb.stream(name)
    }

    pub fn document(&self) -> Result<Vec<u8>> {
        self.cfb.stream(S_DOCUMENT)
    }

    pub fn compression_version(&self) -> Result<String> {
        let v = self.cfb.stream(S_COMPRESSION)?;
        Ok(String::from_utf8_lossy(&v).trim_end_matches('\0').to_string())
    }

    pub fn zip_index(&self) -> Result<Zip> {
        Zip::parse(&self.document()?)
    }

    /// The inverse of parse+copy: a container carrying exactly these streams.
    pub fn build(streams: Vec<(String, Vec<u8>)>) -> Result<Vec<u8>> {
        let mut b = CfbBuilder::new();
        for (k, v) in &streams {
            b.add(k.clone(), v.clone());
        }
        b.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_known_vectors() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"The quick brown fox jumps over the lazy dog"), 0x414F_A339);
    }

    #[test]
    fn deflate_stored_roundtrip_via_reader_semantics() {
        for data in [
            vec![],
            b"x".to_vec(),
            vec![7u8; 70_000], // >1 stored block
        ] {
            let enc = ZipWriter::deflate_stored(&data);
            // stored-block walk (the decode side of BTYPE=00)
            let mut dec = Vec::new();
            let mut p = 0usize;
            loop {
                let hdr = enc[p];
                let last = hdr & 1 == 1;
                assert_eq!(hdr >> 1, 0, "BTYPE must be 00");
                let n = u16le(&enc, p + 1) as usize;
                let n_inv = u16le(&enc, p + 3);
                assert_eq!(n, (!n_inv) as usize, "stored LEN/NLEN");
                dec.extend_from_slice(&enc[p + 5..p + 5 + n]);
                p += 5 + n;
                if last || p >= enc.len() {
                    break;
                }
            }
            assert_eq!(dec, data);
        }
    }

    #[test]
    fn zip_writer_reader_roundtrip() {
        let mut w = ZipWriter::new();
        w.add_stored("a.bin", vec![1, 2, 3, 4, 5]);
        w.add(
            "raw.bin",
            ZipSource::Raw { method: 14, crc: 0x1234, compressed: vec![9, 9, 9], len: 100 },
        );
        w.add_stored("big.bin", vec![0xAA; 200_000]);
        let bytes = w.build().unwrap();
        let z = Zip::parse(&bytes).unwrap();
        assert_eq!(z.entries.len(), 3);
        assert_eq!(z.entries[0].name, "a.bin");
        assert_eq!(z.entries[0].method, 8);
        assert_eq!(z.entries[1].method, 14);
        assert_eq!(z.entries[1].len, 100);
        assert_eq!(z.entries[2].name, "big.bin");
        // raw carry is byte-faithful
        assert_eq!(z.raw(&bytes, 1).unwrap(), vec![9, 9, 9]);
        // crc field is over the UNCOMPRESSED payload; the compressed body is
        // exactly our deterministic stored-block encoding
        let raw0 = z.raw(&bytes, 0).unwrap();
        assert_eq!(raw0, ZipWriter::deflate_stored(&[1, 2, 3, 4, 5]));
        assert_eq!(z.entries[0].crc, crc32(&[1, 2, 3, 4, 5]));
        let raw2 = z.raw(&bytes, 2).unwrap();
        assert_eq!(raw2, ZipWriter::deflate_stored(&vec![0xAA; 200_000]));
        assert_eq!(z.entries[2].crc, crc32(&vec![0xAA; 200_000]));
        assert!(raw2.len() < 200_100);
    }

    #[test]
    fn pkg_container_closes_over_cfb_and_zip() {
        let zip = {
            let mut w = ZipWriter::new();
            w.add_stored("X.dbf", b"PKzippayload".to_vec());
            w.build().unwrap()
        };
        let streams: Vec<(String, Vec<u8>)> = CANONICAL_STREAMS
            .iter()
            .map(|name| {
                (
                    name.to_string(),
                    if *name == S_DOCUMENT {
                        zip.clone()
                    } else if *name == S_COMPRESSION {
                        b"DynaZip5.00.06\0".to_vec()
                    } else {
                        vec![1, 2, 3]
                    },
                )
            })
            .collect();
        let bytes = Pkg::build(streams).unwrap();
        let pkg = Pkg::parse(&bytes).unwrap();
        assert!(pkg.compression_version().unwrap().starts_with(DYNZIP));
        let z2 = pkg.zip_index().unwrap();
        assert_eq!(z2.entries[0].name, "X.dbf");
        assert_eq!(pkg.document().unwrap(), zip);
    }
}

