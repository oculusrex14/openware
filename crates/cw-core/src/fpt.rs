//! FoxPro-style `.FPT` memo sidecar (CaseWare uses it beside every table with
//! M fields; block 0 = header, blocks allocated from `next_free`).
//!
//! Observed anatomy (golden freeze 2026-10-22, hull AC.fpt): `[0..4)` next-free
//! block BE u32, `[6..8)` block size BE u16 (512 throughout the corpus); each
//! block: 4-byte type (1 = text) + 4-byte payload length, payload contiguous
//! from `block*bs + 8` across as many blocks as needed. Files are NOT always
//! block-multiples (hull AC.fpt ends 151 bytes into its last block) — bound
//! reads by actual length.

use crate::error::{CoreError, Result};

#[derive(Debug, Clone)]
pub struct Fpt {
    pub data: Vec<u8>,
    pub block_size: u16,
    pub next_free: u32,
}

impl Fpt {
    pub fn parse(bytes: &[u8]) -> Result<Fpt> {
        if bytes.len() < 8 {
            return Err(CoreError::Format("fpt too small".into()));
        }
        // Empty sidecars exist as bare 512-byte zero blocks (e.g. EC/PR/CE):
        // next_free 0/1, no payloads — still structurally valid.
        Ok(Fpt {
            block_size: u16::from_be_bytes([bytes[6], bytes[7]]),
            next_free: u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            data: bytes.to_vec(),
        })
    }

    pub fn parse_path(path: impl AsRef<std::path::Path>) -> Result<Fpt> {
        let bytes = std::fs::read(path)?;
        Fpt::parse(&bytes)
    }

    /// Verbatim re-emit (C6a). The block layout is carried whole; memo blocks
    /// are never re-packed by the kernel, so `encode(parse(x)) == x`.
    pub fn encode(&self) -> Vec<u8> {
        self.data.clone()
    }

    pub fn write_path(&self, path: impl AsRef<std::path::Path>) -> Result<()> {
        std::fs::write(path, self.encode())?;
        Ok(())
    }

    /// Raw payload of a memo block chain, `None` when out of range.
    pub fn block(&self, block: u32) -> Option<(u32, &[u8])> {
        if block == 0 || self.block_size == 0 {
            return None;
        }
        let base = block as usize * self.block_size as usize;
        if base + 8 > self.data.len() {
            return None;
        }
        let btype = u32::from_be_bytes([
            self.data[base],
            self.data[base + 1],
            self.data[base + 2],
            self.data[base + 3],
        ]);
        let blen = u32::from_be_bytes([
            self.data[base + 4],
            self.data[base + 5],
            self.data[base + 6],
            self.data[base + 7],
        ]);
        let end = (base + 8 + blen as usize).min(self.data.len());
        Some((btype, &self.data[base + 8..end]))
    }

    /// Memo text for a DBF M-field reference (`"0000000123"` style, blank/0 = none).
    pub fn memo(&self, reference: &[u8]) -> Option<&[u8]> {
        let tok = crate::canon::latin1(reference).trim().to_string();
        if tok.is_empty() {
            return None;
        }
        let block: u32 = tok.parse().ok()?;
        self.block(block).map(|(_, payload)| payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_block() {
        // bs=32: block 0 = the 32-byte header, block 1 = our payload block.
        let bs = 32usize;
        let mut data = vec![0u8; bs];
        data[0..4].copy_from_slice(&2u32.to_be_bytes()); // next-free
        data[6..8].copy_from_slice(&(bs as u16).to_be_bytes());
        let mut blk = vec![0u8; bs];
        blk[0..4].copy_from_slice(&1u32.to_be_bytes()); // type = text
        blk[4..8].copy_from_slice(&3u32.to_be_bytes()); // len
        blk[8..11].copy_from_slice(b"abc");
        data.extend_from_slice(&blk);
        let f = Fpt::parse(&data).unwrap();
        assert_eq!(f.block_size, 32);
        assert_eq!(f.next_free, 2);
        let (t, payload) = f.block(1).unwrap();
        assert_eq!(t, 1);
        assert_eq!(payload, b"abc");
        assert!(f.block(2).is_none()); // next-free block, not yet written
        assert!(f.block(9).is_none());
        assert_eq!(f.memo(b"         1"), Some(&b"abc"[..]));
        assert_eq!(f.memo(b"          "), None);
        assert_eq!(f.memo(b"0000000000"), None);
    }
}

