//! LZMA1 decoder (properties-coded, size-capped) + the Caseware pkg member law.
//!
//! F62/F63: every method-14 member of a `CaseWareDocument` zip carries a
//! 19-byte DynaZip prefix; the LZMA1 properties byte sits at offset 18
//! (always 0x5D = lc3/lp0/pb2 in the corpus) and the raw stream follows at
//! 19. No dictionary size is recorded - the decoder sizes its window from
//! the member's uncompressed length (round up to a power of two, min 1<<12,
//! cap 1<<26); a window at least the encoder's is always legal. The stream
//! carries no end marker in the corpus: decoding stops exactly at the
//! member's declared size, then the caller verifies length + crc32.
//!
//! Decoder law = the xz reference config, bit-verified against liblzma on
//! 43/43 random+structured vectors and byte-identical on every clean corpus
//! member (sandbox c6pos/c6corpus): literal-exit `<=3?-3:-6`, SEPARATE
//! match/rep length prob sets, distance bit-trees through slot 13
//! (direct+align only from slot 14), rep-match `reps0=d` with the repN
//! rotation, rc-init skips byte 0, `is_match[state][pos_state]` +
//! `is_rep0_long[state][pos_state]` (xz lzma_decoder.c), the matched-literal
//! subcoder offset narrowing 0x100/0x200/0 (xz range_decoder.h), and the
//! dict-zero rule: distances may reach behind the start, missing history
//! reads 0x00.
//!
//! Corpus gate: `tests/lzma_c6_gate.rs` (fixtures `tests/golden/pkg/lzma.json`,
//! generator `examples/freeze_lzma.rs`). Known residual: a minority of corpus
//! members stop early even under the reference decoder - tracked in F63; the
//! gate pins the clean counts so regressions stay visible.
//!
//! F63 measured (2026-10-28 rebuild): the damaged members are genuinely
//! truncated/corrupt streams, not a decoder gap. liblzma (xz) aborts with
//! "Corrupt input data" at the same points and after the same output prefix
//! our decoder produces - e.g. hull2024 MP after 1,055,945/3,781,062 bytes,
//! BL after 489,701/883,452, TP after 385,778/6,820,540. Our [`Lz1`] instead
//! zero-fills: [`RangeDec::next_byte`] returns 0 past the end of the buffer, so
//! a truncated member decodes to its declared size with fabricated tail bytes
//! (the `.dbf` fixtures inherit those zeros). The gate therefore classifies a
//! member `clean` only when the decoded length AND the zip's stored crc32 both
//! match; everything else is `partial` and only its count is contractual.

use crate::error::{CoreError, Result};

const NS: usize = 12; // states
const ALIGN_BITS: usize = 4;
const MATCH_MIN: usize = 2;
const LEN_TO_POS_STATES: usize = 4;
/// xz `POS_STATES_MAX` (1 << LZMA_PB_MAX). `is_match` and `is_rep0_long` carry
/// a pos_state dimension: `[state][pos_state]` (lzma_decoder.c:353/566).
const POS_STATES: usize = 16;

const INIT_PROB: u16 = 1024;

// ---------------------------------------------------------------------------
// range coder
// ---------------------------------------------------------------------------

struct RangeDec<'a> {
    buf: &'a [u8],
    pos: usize,
    range: u32,
    code: u32,
}

impl<'a> RangeDec<'a> {
    /// Byte 0 of the stream is the discarded init byte; code = next 4 BE.
    fn new(buf: &'a [u8]) -> RangeDec<'a> {
        let mut code = 0u32;
        for i in 1..5 {
            code = (code << 8) | *buf.get(i).unwrap_or(&0) as u32;
        }
        RangeDec { buf, pos: 5, range: 0xFFFF_FFFF, code }
    }

    #[inline]
    fn next_byte(&mut self) -> u32 {
        let b = *self.buf.get(self.pos).unwrap_or(&0) as u32;
        self.pos += 1;
        b
    }

    #[inline]
    fn normalize(&mut self) {
        if self.range < 1 << 24 {
            self.range <<= 8;
            let b = self.next_byte();
            self.code = (self.code << 8) | b;
        }
    }

    #[inline]
    fn bit(&mut self, prob: &mut u16) -> u32 {
        self.normalize();
        let bound = (self.range >> 11) * (*prob as u32);
        let sym;
        if self.code < bound {
            *prob += (2048 - *prob) >> 5;
            self.range = bound;
            sym = 0;
        } else {
            *prob -= *prob >> 5;
            self.code -= bound;
            self.range -= bound;
            sym = 1;
        }
        self.normalize();
        sym
    }

    fn tree(&mut self, probs: &mut [u16], bits: usize) -> usize {
        let mut m = 1usize;
        for _ in 0..bits {
            let b = self.bit(&mut probs[m]);
            m = (m << 1) | b as usize;
        }
        m - (1 << bits)
    }

    fn reversed(&mut self, probs: &mut [u16], bits: usize) -> u32 {
        // LZMA SDK ReverseDecode: walk the same node tree as forward bits,
        // accumulate the bit value LSB-first (prob lives at the node).
        let mut sym = 0u32;
        let mut m = 1usize;
        for i in 0..bits {
            let b = self.bit(&mut probs[m]);
            m = (m << 1) | b as usize;
            if b == 1 {
                sym |= 1 << i;
            }
        }
        sym
    }

    fn direct(&mut self, n: usize) -> u32 {
        // xz convention: normalize first, then halve; no-borrow => bit 1
        let mut result = 0u32;
        for _ in 0..n {
            self.normalize();
            self.range >>= 1;
            let code = self.code.wrapping_sub(self.range);
            if (code >> 31) == 0 {
                // no borrow: bit 1, keep the subtracted residual
                self.code = code;
                result = (result << 1) + 1;
            } else {
                // borrow: bit 0, restore
                self.code = code.wrapping_add(self.range);
                result <<= 1;
            }
        }
        result
    }
}

// ---------------------------------------------------------------------------
// model + decoder
// ---------------------------------------------------------------------------

pub struct Lz1 {
    lc: u32,
    lp: u32,
    pb: u32,
    state: usize,
    reps: [usize; 4],
    lit: Vec<u16>, // 0x300 << (lc+lp)
    is_match: [u16; NS * POS_STATES],
    is_rep: [u16; NS],
    is_rep_g0: [u16; NS],
    is_rep_g1: [u16; NS],
    is_rep_g2: [u16; NS],
    is_rep0_long: [u16; NS * POS_STATES],
    pos_slot: [[u16; 64]; LEN_TO_POS_STATES],
    /// per-slot bit trees for slots 4..=13 (xz law: direct+align from slot 14)
    dist_trees: [Vec<u16>; 10],
    align: [u16; 1 << ALIGN_BITS],
    /// match-length and (xz-law) separate rep-length prob sets
    len_choice: u16,
    len_choice2: u16,
    len_low: [[u16; 8]; 16],
    len_mid: [[u16; 8]; 16],
    len_high: [u16; 256],
    rep_choice: u16,
    rep_choice2: u16,
    rep_low: [[u16; 8]; 16],
    rep_mid: [[u16; 8]; 16],
    rep_high: [u16; 256],
}

impl Lz1 {
    pub fn new(lc: u32, lp: u32, pb: u32) -> Lz1 {
        let mut dist_trees: [Vec<u16>; 10] = Default::default();
        for slot in 4..14 {
            let ndb = (slot >> 1) - 1;
            dist_trees[slot - 4] = vec![INIT_PROB; 1 << ndb];
        }
        Lz1 {
            lc,
            lp,
            pb,
            state: 0,
            reps: [0; 4],
            lit: vec![INIT_PROB; 0x300usize << (lc + lp)],
            is_match: [INIT_PROB; NS * POS_STATES],
            is_rep: [INIT_PROB; NS],
            is_rep_g0: [INIT_PROB; NS],
            is_rep_g1: [INIT_PROB; NS],
            is_rep_g2: [INIT_PROB; NS],
            is_rep0_long: [INIT_PROB; NS * POS_STATES],
            pos_slot: [[INIT_PROB; 64]; LEN_TO_POS_STATES],
            dist_trees,
            align: [INIT_PROB; 1 << ALIGN_BITS],
            len_choice: INIT_PROB,
            len_choice2: INIT_PROB,
            len_low: [[INIT_PROB; 8]; 16],
            len_mid: [[INIT_PROB; 8]; 16],
            len_high: [INIT_PROB; 256],
            rep_choice: INIT_PROB,
            rep_choice2: INIT_PROB,
            rep_low: [[INIT_PROB; 8]; 16],
            rep_mid: [[INIT_PROB; 8]; 16],
            rep_high: [INIT_PROB; 256],
        }
    }
}

impl Lz1 {
    /// Decode exactly `limit` bytes; error if the input ends first.
    pub fn decode(&mut self, src: &[u8], limit: usize) -> Result<Vec<u8>> {
        let mut rc = RangeDec::new(src);
        let mut out: Vec<u8> = Vec::with_capacity(limit);
        let pb_mask = (1usize << self.pb) - 1;
        let lp_mask = (1usize << self.lp) - 1;

        while out.len() < limit {
            let pos = out.len();
            let pos_state = pos & pb_mask;
            // is_match carries pos_state (xz: is_match[state][pos_state])
            let im_idx = self.state * POS_STATES + pos_state;
            if rc.bit(&mut self.is_match[im_idx]) == 0 {
                // ----- literal -----
                let prev = if pos > 0 { out[pos - 1] } else { 0 };
                let ls = ((pos & lp_mask) << self.lc)
                    | ((prev as usize) >> (8 - self.lc as usize));
                let base = 0x300 * ls;
                let mut sym = 1usize;
                if self.state >= 7 {
                    let dist0 = self.reps[0];
                    // dict-zero law: history behind the start reads 0x00
                    let mut match_byte =
                        if dist0 < pos { out[pos - dist0 - 1] as u32 } else { 0 };
                    // xz rc_matched_literal (range_decoder.h): the subcoder
                    // offset is 0x100 while the decoded bits still match the
                    // match byte, 0x200 while matched with bit 1, and 0 once
                    // they diverge - not a fixed 0x100/0x200 split.
                    let mut off = 0x100usize;
                    for _ in 0..8 {
                        match_byte <<= 1;
                        let mbn = (match_byte as usize) & off;
                        let idx = base + off + mbn + sym;
                        let bit = rc.bit(&mut self.lit[idx]);
                        sym = (sym << 1) | bit as usize;
                        off = if bit == 0 { off & !mbn } else { off & mbn };
                    }
                } else {
                    for _ in 0..8 {
                        let idx = base + sym;
                        let bit = rc.bit(&mut self.lit[idx]);
                        sym = (sym << 1) | bit as usize;
                    }
                }
                let byte = (sym & 0xFF) as u8;
                out.push(byte);
                // xz law (c6final 'le3'): only <=3 exits to zero
                self.state = if self.state <= 3 {
                    0
                } else if self.state <= 9 {
                    self.state - 3
                } else {
                    self.state - 6
                };
                continue;
            }

            // ----- match / replicated match -----
            let len;
            let dist;
            if rc.bit(&mut self.is_rep[self.state]) == 0 {
                len = self.len_decode(&mut rc, pos_state, false);
                dist = self.dist_decode(&mut rc, len)? as usize;
                self.reps[3] = self.reps[2];
                self.reps[2] = self.reps[1];
                self.reps[1] = self.reps[0];
                self.reps[0] = dist;
                self.state = if self.state < 7 { 7 } else { 10 };
            } else if rc.bit(&mut self.is_rep_g0[self.state]) == 0 {
                if rc.bit(&mut self.is_rep0_long[im_idx]) == 0 {
                    // short rep: single byte at rep0
                    self.state = if self.state < 7 { 9 } else { 11 };
                    copy_match(&mut out, self.reps[0], 1)?;
                    continue;
                }
                len = self.len_decode(&mut rc, pos_state, true);
                self.state = if self.state < 7 { 8 } else { 11 };
                copy_match(&mut out, self.reps[0], len)?;
                continue;
            } else {
                let (d, ri);
                if rc.bit(&mut self.is_rep_g1[self.state]) == 0 {
                    d = self.reps[1];
                    ri = 1;
                } else if rc.bit(&mut self.is_rep_g2[self.state]) == 0 {
                    d = self.reps[2];
                    ri = 2;
                } else {
                    d = self.reps[3];
                    ri = 3;
                }
                // xz law: using repN shifts reps[0..N] down by one and puts d
                // at rep0; entries deeper than N keep their distance.
                for j in (1..=ri).rev() {
                    self.reps[j] = self.reps[j - 1];
                }
                self.reps[0] = d;
                dist = d;
                len = self.len_decode(&mut rc, pos_state, true);
                self.state = if self.state < 7 { 8 } else { 11 };
            }
            copy_match(&mut out, dist, len)?;
        }
        Ok(out)
    }

    fn len_decode(&mut self, rc: &mut RangeDec, pos_state: usize, rep: bool) -> usize {
        // xz law: match-len and rep-len are SEPARATE prob sets (c6final 'split')
        let (choice, choice2, low, mid, high) = if rep {
            (
                &mut self.rep_choice,
                &mut self.rep_choice2,
                &mut self.rep_low,
                &mut self.rep_mid,
                &mut self.rep_high,
            )
        } else {
            (
                &mut self.len_choice,
                &mut self.len_choice2,
                &mut self.len_low,
                &mut self.len_mid,
                &mut self.len_high,
            )
        };
        if rc.bit(choice) == 0 {
            MATCH_MIN + rc.tree(&mut low[pos_state], 3)
        } else if rc.bit(choice2) == 0 {
            MATCH_MIN + 8 + rc.tree(&mut mid[pos_state], 3)
        } else {
            MATCH_MIN + 16 + rc.tree(high, 8)
        }
    }

    fn dist_decode(&mut self, rc: &mut RangeDec, len: usize) -> Result<u32> {
        let len_state = (len - MATCH_MIN).min(LEN_TO_POS_STATES - 1);
        let slot = rc.tree(&mut self.pos_slot[len_state], 6);
        if slot < 4 {
            return Ok(slot as u32);
        }
        let ndb = (slot >> 1) - 1;
        let mut dist = ((2 | (slot & 1)) << ndb) as u32;
        if slot < 14 {
            // xz law: dedicated bit-trees through slot 13 (c6final distcut 14)
            let low = rc.reversed(&mut self.dist_trees[slot - 4], ndb);
            dist += low;
        } else {
            let high = rc.direct(ndb - ALIGN_BITS);
            dist |= (high << ALIGN_BITS) | rc.reversed(&mut self.align, ALIGN_BITS);
        }
        Ok(dist)
    }
}

fn copy_match(out: &mut Vec<u8>, dist: usize, mut len: usize) -> Result<()> {
    // lzma distances are 0-based (dist 0 = previous byte); the LZMA dict is
    // zero-filled - distances behind the start read 0x00 (c6final copy law)
    let mut idx = out.len() as isize - dist as isize - 1;
    for _ in 0..len {
        let b = if idx >= 0 && (idx as usize) < out.len() {
            out[idx as usize]
        } else {
            0
        };
        out.push(b);
        idx += 1;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// public API
// ---------------------------------------------------------------------------

/// Window size for a member: >= 1<<12, power of two, cap 1<<26. A window
/// larger than the encoder's decodes identically; this only bounds memory.
#[allow(dead_code)]
fn window_for(uncompressed: usize) -> u32 {
    let mut w = 1u32 << 12;
    while (w as usize) < uncompressed && w < 1 << 26 {
        w <<= 1;
    }
    w
}

/// Split a properties byte into (lc, lp, pb) - xz convention:
/// `props = lc + 9*(lp + 5*pb)`.
pub fn props_split(props_byte: u8) -> Result<(u32, u32, u32)> {
    let lc = (props_byte % 9) as u32;
    let t = props_byte / 9;
    let lp = (t % 5) as u32;
    let pb = (t / 5) as u32;
    if lc + lp > 8 {
        return Err(CoreError::Format(format!("lzma: props {props_byte} invalid")));
    }
    Ok((lc, lp, pb))
}

/// Decode a raw LZMA1 stream (no header) to exactly `uncompressed` bytes.
/// The stream may carry an end marker; decoding stops at `uncompressed`.
pub fn decode_raw(src: &[u8], props_byte: u8, uncompressed: usize) -> Result<Vec<u8>> {
    let (lc, lp, pb) = props_split(props_byte)?;
    // A range-coded stream that can produce output needs the 5-byte rc init
    // (leading byte + 4 code bytes). Shorter input is a truncation, not a
    // zero-filled stream: report it instead of fabricating history bytes.
    if uncompressed > 0 && src.len() < 5 {
        return Err(CoreError::Format(format!(
            "lzma: stream of {} bytes cannot hold {uncompressed} bytes",
            src.len()
        )));
    }
    let mut dec = Lz1::new(lc, lp, pb);
    let out = dec.decode(src, uncompressed)?;
    if out.len() != uncompressed {
        return Err(CoreError::Format(format!(
            "lzma: decoded {} of {uncompressed}",
            out.len()
        )));
    }
    Ok(out)
}

/// The pkg law (F63): strip the 19-byte DynaZip prefix - properties byte at
/// [18], stream from [19] - and LZMA1-decode to the member's declared size.
pub fn decode_pkg_member(comp: &[u8], uncompressed: usize) -> Result<Vec<u8>> {
    if comp.len() < 20 {
        return Err(CoreError::Format("lzma: member too short".into()));
    }
    decode_raw(&comp[19..], comp[18], uncompressed)
}

/// Restore-side variant of [`decode_pkg_member`]: the last match of a member
/// may emit past the declared size (a match is up to 273 bytes), and the
/// corpus' damaged minority overshoots by a few hundred bytes; CaseWare's own
/// expansion truncates at the declared size, so restore parity does the same.
pub fn decode_pkg_member_truncating(comp: &[u8], uncompressed: usize) -> Result<Vec<u8>> {
    if uncompressed == 0 {
        return Ok(Vec::new());
    }
    if comp.len() < 20 {
        return Err(CoreError::Format("lzma: member too short".into()));
    }
    let (lc, lp, pb) = props_split(comp[18])?;
    let mut dec = Lz1::new(lc, lp, pb);
    let mut out = dec.decode(&comp[19..], uncompressed)?;
    out.truncate(uncompressed);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn props_split_known() {
        assert_eq!(props_split(0x5D).unwrap(), (3, 0, 2));
        assert_eq!(props_split(0).unwrap(), (0, 0, 0));
        assert!(props_split(0x3F + 9 * 9).is_ok());
    }

    #[test]
    fn window_bounds() {
        assert_eq!(window_for(0), 1 << 12);
        assert_eq!(window_for(1 << 12), 1 << 12);
        assert_eq!(window_for((1 << 12) + 1), 1 << 13);
        assert_eq!(window_for(1 << 30), 1 << 26);
    }

    #[test]
    fn empty_stream_roundtrip_shape() {
        // An all-0x00 5-byte init with no data decodes nothing; the gate
        // carries the real vectors - this only pins the API shape.
        let err = decode_raw(&[], 0x5D, 1).unwrap_err();
        assert!(matches!(err, CoreError::Format(_)));
    }
}

