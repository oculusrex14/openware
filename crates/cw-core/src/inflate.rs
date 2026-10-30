//! DEFLATE (RFC 1951) decoder — ZIP method 8.
//!
//! The pkg writer emits only stored blocks (`pkg::ZipWriter::deflate_stored`),
//! but CaseWare's own containers compress their 5-byte hub stub with a real
//! deflate stream and third-party members can be deflated too, so the read
//! path needs the full law. Table-driven canonical Huffman; no dictionaries,
//! no zlib wrapper (raw deflate only — ZIP framing supplies the CRC).
//!
//! Gate: `crates/cw-core/tests/backup_gate.rs` decodes the hub stub of both
//! frozen packages and every method-8 member to its declared size.

use crate::{CoreError, Result};

fn fmt<T>(msg: impl Into<String>) -> Result<T> {
    Err(CoreError::Format(msg.into()))
}

struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
    bit: u32,
}

impl<'a> Bits<'a> {
    fn new(b: &'a [u8]) -> Self {
        Bits { b, pos: 0, bit: 0 }
    }

    fn bit(&mut self) -> Result<u32> {
        if self.pos >= self.b.len() {
            return fmt("deflate: out of input");
        }
        let v = (self.b[self.pos] >> self.bit) & 1;
        self.bit += 1;
        if self.bit == 8 {
            self.bit = 0;
            self.pos += 1;
        }
        Ok(v as u32)
    }

    fn bits(&mut self, n: u32) -> Result<u32> {
        let mut v = 0u32;
        for i in 0..n {
            v |= self.bit()? << i;
        }
        Ok(v)
    }

    fn align(&mut self) {
        if self.bit != 0 {
            self.bit = 0;
            self.pos += 1;
        }
    }

    /// Peek `n` bits LSB-first, zero-padding past end of input (only legal for
    /// the trailing padding region; a real over-read still trips `drop`).
    fn peek(&self, n: u32) -> u32 {
        let mut v = 0u32;
        for i in 0..n {
            let abs = self.pos * 8 + self.bit as usize + i as usize;
            let byte = abs / 8;
            let off = abs % 8;
            let b = self.b.get(byte).copied().unwrap_or(0);
            v |= (((b >> off) & 1) as u32) << i;
        }
        v
    }

    fn drop_bits(&mut self, n: u32) {
        let abs = self.pos * 8 + self.bit as usize + n as usize;
        self.pos = abs / 8;
        self.bit = (abs % 8) as u32;
    }
}

struct Huff {
    /// `table[(reversed_code) | (k << len)] = symbol`
    table: Vec<i16>,
    lens: Vec<u8>,
    max: u32,
}

fn reverse(mut v: u32, n: u32) -> u32 {
    let mut r = 0;
    for _ in 0..n {
        r = (r << 1) | (v & 1);
        v >>= 1;
    }
    r
}

fn build(lens: &[u8]) -> Result<Huff> {
    let max = *lens.iter().max().unwrap_or(&0) as u32;
    if max == 0 || max > 15 {
        return fmt(format!("deflate: bad huffman depth {max}"));
    }
    let mut bl_count = [0u32; 16];
    for &l in lens {
        bl_count[l as usize] += 1;
    }
    bl_count[0] = 0;
    let mut next = [0u32; 16];
    let mut code = 0u32;
    for bits in 1..=15usize {
        code = (code + bl_count[bits - 1]) << 1;
        next[bits] = code;
    }
    let mut table = vec![-1i16; 1usize << max];
    for (sym, &l) in lens.iter().enumerate() {
        if l == 0 {
            continue;
        }
        let l = l as u32;
        let c = next[l as usize];
        next[l as usize] += 1;
        let rev = reverse(c, l);
        let step = 1usize << l;
        let mut i = rev as usize;
        while i < table.len() {
            table[i] = sym as i16;
            i += step;
        }
    }
    Ok(Huff { table, lens: lens.to_vec(), max })
}

impl Huff {
    fn decode(&self, br: &mut Bits<'_>) -> Result<usize> {
        let idx = br.peek(self.max) as usize;
        let sym = self.table.get(idx).copied().unwrap_or(-1);
        if sym < 0 {
            return fmt("deflate: invalid huffman code");
        }
        let len = self.lens[sym as usize] as u32;
        br.drop_bits(len);
        Ok(sym as usize)
    }
}

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
    131, 163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
    13, 13,
];

fn fixed() -> Result<(Huff, Huff)> {
    let mut lit = vec![0u8; 288];
    for (i, l) in lit.iter_mut().enumerate() {
        *l = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    Ok((build(&lit)?, build(&[5u8; 30])?))
}

fn dynamic(br: &mut Bits<'_>) -> Result<(Huff, Huff)> {
    let hlit = br.bits(5)? as usize + 257;
    let hdist = br.bits(5)? as usize + 1;
    let hclen = br.bits(4)? as usize + 4;
    const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
    let mut cl = [0u8; 19];
    for i in 0..hclen {
        cl[ORDER[i]] = br.bits(3)? as u8;
    }
    let clh = build(&cl)?;
    let mut lens: Vec<u8> = Vec::with_capacity(hlit + hdist);
    while lens.len() < hlit + hdist {
        let sym = clh.decode(br)?;
        match sym {
            0..=15 => lens.push(sym as u8),
            16 => {
                let prev = *lens.last().ok_or_else(|| CoreError::Format("deflate: repeat first".into()))?;
                let n = 3 + br.bits(2)? as usize;
                for _ in 0..n {
                    lens.push(prev);
                }
            }
            17 => {
                let n = 3 + br.bits(3)? as usize;
                lens.extend(std::iter::repeat(0).take(n));
            }
            18 => {
                let n = 11 + br.bits(7)? as usize;
                lens.extend(std::iter::repeat(0).take(n));
            }
            _ => return fmt("deflate: bad code-length symbol"),
        }
    }
    lens.truncate(hlit + hdist);
    let (lit_lens, dist_lens) = lens.split_at(hlit);
    Ok((build(lit_lens)?, build(dist_lens)?))
}

fn block(br: &mut Bits<'_>, out: &mut Vec<u8>, lit: &Huff, dist: &Huff, limit: usize) -> Result<()> {
    loop {
        let sym = lit.decode(br)?;
        if sym < 256 {
            out.push(sym as u8);
        } else if sym == 256 {
            return Ok(());
        } else {
            let li = sym - 257;
            if li >= LEN_BASE.len() {
                return fmt("deflate: bad length symbol");
            }
            let len = LEN_BASE[li] as usize + br.bits(LEN_EXTRA[li] as u32)? as usize;
            let ds = dist.decode(br)?;
            if ds >= DIST_BASE.len() {
                return fmt("deflate: bad distance symbol");
            }
            let d = DIST_BASE[ds] as usize + br.bits(DIST_EXTRA[ds] as u32)? as usize;
            if d > out.len() {
                return fmt("deflate: distance past output");
            }
            let start = out.len() - d;
            for i in 0..len {
                let b = out[start + i];
                out.push(b);
            }
        }
        if out.len() > limit {
            return fmt(format!("deflate: output exceeds {limit}"));
        }
    }
}

/// Raw deflate decode; `expected` is a hard output cap (ZIP's uncompressed size).
pub fn inflate(src: &[u8], expected: usize) -> Result<Vec<u8>> {
    let cap = if expected == 0 { usize::MAX } else { expected };
    let mut br = Bits::new(src);
    let mut out = Vec::with_capacity(expected.min(1 << 20));
    loop {
        let last = br.bit()? == 1;
        match br.bits(2)? {
            0 => {
                br.align();
                let len = br.bits(16)? as usize;
                let nlen = br.bits(16)? as usize;
                if len != (!nlen & 0xFFFF) {
                    return fmt("deflate: stored LEN/NLEN mismatch");
                }
                for _ in 0..len {
                    out.push(br.bits(8)? as u8);
                }
            }
            1 => {
                let (lit, dist) = fixed()?;
                block(&mut br, &mut out, &lit, &dist, cap)?;
            }
            2 => {
                let (lit, dist) = dynamic(&mut br)?;
                block(&mut br, &mut out, &lit, &dist, cap)?;
            }
            _ => return fmt("deflate: reserved BTYPE"),
        }
        if out.len() > cap {
            return fmt(format!("deflate: output exceeds {cap}"));
        }
        if last {
            return Ok(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::ZipWriter;

    #[test]
    fn stored_blocks_roundtrip() {
        for data in [vec![], b"abc".to_vec(), vec![0x5Au8; 70_000]] {
            let enc = ZipWriter::deflate_stored(&data);
            assert_eq!(inflate(&enc, data.len()).unwrap(), data);
        }
    }

    #[test]
    fn fixed_huffman_stub() {
        // deflate of b"\x058.98" as emitted by CaseWare's package hub
        let enc = [0x63u8, 0xb5, 0xd0, 0xb3, 0xb4, 0x00, 0x00];
        assert_eq!(inflate(&enc, 5).unwrap(), b"\x058.98");
    }

    #[test]
    fn rejects_garbage() {
        assert!(inflate(&[0xFF, 0xFF, 0xFF], 16).is_err());
    }
}

