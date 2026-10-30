//! The CWGUID object id — P5.C2 (F59).
//!
//! Every core row carries a 22-char `CWGUID` acting as the opaque primary key
//! of the interlink web (F32). Empirical law (57,451 distinct ids; hull2024 +
//! corp2026 + mwgh2025 golden corpora and COM-created fixture rows, 2026-10-22):
//!
//! * 22 chars, alphabet `A-Za-z0-9` plus `+` (value 62) and `-` (value 63);
//!   `/` and `_` never occur. The 22 sextets form one big-endian 132-bit
//!   value, MSB-first — there is no base64 padding: the last char carries 4
//!   trailing data bits, uniform-random in the corpus (probe `c2tail`).
//!
//! * The high 128 bits (`data`, big-endian bytes) are constrained:
//!   - global: `data[0] & 0xC0 == 0`, `data[8] & 0x30 == 0`,
//!     `data[15] & 0x0C == 0x08`
//!   - family B tables (AN GL HI MP SH TS TM): `data[6] & 0x3C == 0x10`
//!   - family C tables (CV FP TP WR): `data[7] & 0x3C == 0x0C` and
//!     `data[15] & 0x0F == 0x08`
//!   - family A tables (AC AM BL): global constraints only.
//!   The tagged bytes have exactly 16 distinct values corpus-wide — 4-bit
//!   type fields, identical across the 2024/2025/2026 generations.
//!
//! * ids are unique within a table in the steady state; the corpus's one
//!   exception is CW itself reusing an id when regenerating a document
//!   (corp2026/SH: two `LS` rows share `GCFPaLwcEs2BuPz5f54tCC`). No
//!   timestamp ordering (rank correlation vs CWDATE = 0.5, probe `c2guid2`).
//!
//! The allocator fixes the family mask and randomizes the remaining
//! 122/118/116 bits. Parse is structural-lenient (length + alphabet); the
//! family-mask check is [`CwGuid::matches`], asserted by [`audit`] and the
//! golden tests.

use crate::error::{CoreError, Result};

/// Length of every CWGUID, in chars.
pub const LEN: usize = 22;

/// The 64-symbol alphabet; index = 6-bit value (`+`=62, `-`=63; no `/`, `_`).
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+-";

/// Object-family tag observed on the id (F59); `A` is the untagged base form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Plain form: AC, AM, BL.
    A,
    /// `data[6]` tag `0x10`: AN GL HI MP SH TS TM.
    B,
    /// `data[7]` tag `0x0C` + tight `data[15]`: CV FP TP WR.
    C,
}

impl Family {
    /// Table -> family map from the corpus census. Tables with no observed
    /// rows (CE EC GR GS PR RA WT SU MSD WP RL..) are `None` — allocator
    /// callers fall back to [`Family::A`] (see `docs\specs\guid.md` unknowns).
    pub fn for_table(table: &str) -> Option<Family> {
        match table {
            "AC" | "AM" | "BL" => Some(Family::A),
            "AN" | "GL" | "HI" | "MP" | "SH" | "TS" | "TM" => Some(Family::B),
            "CV" | "FP" | "TP" | "WR" => Some(Family::C),
            _ => None,
        }
    }

    fn apply(self, data: &mut [u8; 16]) {
        data[0] &= !0xC0;
        data[8] &= !0x30;
        data[15] = (data[15] & !0x0C) | 0x08;
        match self {
            Family::A => {}
            Family::B => data[6] = (data[6] & !0x3C) | 0x10,
            Family::C => {
                data[7] = (data[7] & !0x3C) | 0x0C;
                data[15] = (data[15] & !0x0F) | 0x08;
            }
        }
    }

    fn accepts(self, data: &[u8; 16]) -> bool {
        let global = data[0] & 0xC0 == 0
            && data[8] & 0x30 == 0
            && data[15] & 0x0C == 0x08;
        match self {
            Family::A => global,
            Family::B => global && data[6] & 0x3C == 0x10,
            Family::C => global && data[7] & 0x3C == 0x0C && data[15] & 0x0F == 0x08,
        }
    }
}

/// A parsed CWGUID: high 128 bits + the 4 trailing stream bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CwGuid {
    data: [u8; 16],
    tail: u8,
}

impl CwGuid {
    /// Structural-lenient parse: length + alphabet only. Family masks are a
    /// separate [`matches`](Self::matches) so the reader tolerates any id
    /// CW itself wrote.
    pub fn parse(s: &str) -> Result<CwGuid> {
        let b = s.as_bytes();
        if b.len() != LEN {
            return Err(CoreError::Format(format!(
                "cwguid len {} != {LEN}",
                b.len()
            )));
        }
        let v = |i: usize| -> Result<u8> {
            ALPHABET
                .iter()
                .position(|a| *a == b[i])
                .map(|p| p as u8)
                .ok_or_else(|| {
                    CoreError::Format(format!("cwguid byte {i} ({}) not in alphabet", b[i] as char))
                })
        };
        let mut data = [0u8; 16];
        for blk in 0..5 {
            let (v0, v1, v2, v3) = (v(4 * blk)?, v(4 * blk + 1)?, v(4 * blk + 2)?, v(4 * blk + 3)?);
            data[3 * blk] = (v0 << 2) | (v1 >> 4);
            data[3 * blk + 1] = ((v1 & 0xF) << 4) | (v2 >> 2);
            data[3 * blk + 2] = ((v2 & 0x3) << 6) | v3;
        }
        let (v20, v21) = (v(20)?, v(21)?);
        data[15] = (v20 << 2) | (v21 >> 4);
        Ok(CwGuid { data, tail: v21 & 0xF })
    }

    /// Parse a DBF cell (space-padded latin-1; ascii here).
    pub fn parse_cell(cell: &[u8]) -> Result<CwGuid> {
        let s: String = crate::canon::latin1(cell).into();
        Self::parse(s.trim())
    }

    /// The canonical 22-char form.
    pub fn encode(&self) -> String {
        let mut out = String::with_capacity(LEN);
        for blk in 0..5 {
            let (d0, d1, d2) = (self.data[3 * blk], self.data[3 * blk + 1], self.data[3 * blk + 2]);
            out.push(char_at(((d0 >> 2) as usize)));
            out.push(char_at(((((d0 & 0x3) as usize) << 4) | (d1 >> 4) as usize)));
            out.push(char_at(((((d1 & 0xF) as usize) << 2) | (d2 >> 6) as usize)));
            out.push(char_at((d2 & 0x3F) as usize));
        }
        let d15 = self.data[15];
        out.push(char_at((d15 >> 2) as usize));
        out.push(char_at(((((d15 & 0x3) as usize) << 4) | self.tail as usize) & 0x3F));
        out
    }

    pub fn data(&self) -> &[u8; 16] {
        &self.data
    }

    pub fn tail(&self) -> u8 {
        self.tail
    }

    /// Full structural law incl. the family tag (F59).
    pub fn matches(&self, family: Family) -> bool {
        family.accepts(&self.data)
    }

    /// Fresh id in the family CW's own allocator uses for `table`
    /// (unobserved tables fall back to [`Family::A`], F59 unknowns).
    pub fn allocate_for_table(table: &str, rng: &mut Rng) -> CwGuid {
        Self::allocate(Family::for_table(table).unwrap_or(Family::A), rng)
    }

    /// Fresh id: family mask fixed, remaining bits random.
    pub fn allocate(family: Family, rng: &mut Rng) -> CwGuid {
        let mut data = [0u8; 16];
        data[..8].copy_from_slice(&rng.next().to_be_bytes());
        data[8..].copy_from_slice(&rng.next().to_be_bytes());
        let tail = (rng.next() & 0xF) as u8;
        family.apply(&mut data);
        let id = CwGuid { data, tail };
        debug_assert!(id.matches(family), "allocator mask bug");
        id
    }
}

fn char_at(v: usize) -> char {
    ALPHABET[v] as char
}

impl std::fmt::Display for CwGuid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.encode())
    }
}

/// Process-unique xorshift64*; seed mixed from the wall clock once.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        Rng((seed ^ nanos) | 1)
    }

    /// Deterministic constructor for fixtures/golden gates: same seed ⇒ same
    /// stream (no clock mixing). Production callers keep [`Rng::new`].
    pub fn new_seeded(seed: u64) -> Rng {
        Rng(seed | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

impl Default for Rng {
    fn default() -> Self {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Rng::new(0x9E37_79B9_7F4A_7C15 ^ n.wrapping_mul(0xBF58_476D_1CE4_E5B9))
    }
}

/// Integrity census of a table's `CWGUID` column (P5.C2 gate): structural
/// failures and in-table duplicates. Blank cells are counted, not errors.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Audit {
    pub rows: usize,
    pub blank: usize,
    /// non-blank ids that are not the current 22-char form (observed: the
    /// factory-seeded corp2026/HI `FACCUSR` row `+jFTqvGp`, 2021 template).
    pub legacy: usize,
    pub malformed: usize,
    pub bad_family: usize,
    pub duplicates: usize,
}

pub const CWGUID_COL: &str = "CWGUID";

/// Audit every live record (deleted rows are tombstones: the corpus law and
/// the Python census both read `skip_deleted`, so uniqueness is asserted on
/// live rows only).
pub fn audit(table: &crate::dbf::Table, family: Option<Family>) -> Audit {
    let mut a = Audit {
        rows: table.count(),
        ..Audit::default()
    };
    if table.field_index(CWGUID_COL).is_none() {
        return a;
    }
    let mut seen = std::collections::HashSet::new();
    for r in 0..table.count() {
        if table.is_deleted(r) {
            continue;
        }
        let raw = match table.cell(r, CWGUID_COL) {
            Some(c) => c,
            None => continue,
        };
        let s: String = crate::canon::latin1(raw).into();
        let s = s.trim();
        if s.is_empty() {
            a.blank += 1;
            continue;
        }
        if s.len() != LEN {
            a.legacy += 1;
            continue;
        }
        match CwGuid::parse(s) {
            Ok(id) => {
                if family.map_or(false, |f| !id.matches(f)) {
                    a.bad_family += 1;
                }
                if !seen.insert(id) {
                    a.duplicates += 1;
                }
            }
            Err(_) => a.malformed += 1,
        }
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL_A: &str = "JiMOCu8PaU8NAefTc8LtWM"; // AC row, mwgh2025
    const REAL_B: &str = "D32ZB7OtE-qDpVPoZIhhuS"; // interlink-model example
    const REAL_C: &str = "MWQRYM5UbY4KSODcbKesiM"; // AM row, mwgh2025

    #[test]
    fn parse_encode_roundtrip() {
        for s in [REAL_A, REAL_B, REAL_C] {
            let id = CwGuid::parse(s).unwrap();
            assert_eq!(id.encode(), s);
        }
    }

    #[test]
    fn rejects_bad_len_and_alphabet() {
        assert!(CwGuid::parse("short").is_err());
        let mut s = REAL_A.to_string();
        s.replace_range(0..1, "!");
        assert!(CwGuid::parse(&s).is_err());
    }

    #[test]
    fn family_table_map() {
        assert_eq!(Family::for_table("AM"), Some(Family::A));
        assert_eq!(Family::for_table("HI"), Some(Family::B));
        assert_eq!(Family::for_table("TP"), Some(Family::C));
        assert_eq!(Family::for_table("ZZ"), None);
    }

    #[test]
    fn allocate_obey_masks_and_stay_unique() {
        let mut rng = Rng::default();
        for fam in [Family::A, Family::B, Family::C] {
            let mut seen = std::collections::HashSet::new();
            for _ in 0..10_000 {
                let id = CwGuid::allocate(fam, &mut rng);
                assert!(id.matches(fam), "{id} violates {fam:?}");
                assert_eq!(id.encode().len(), LEN);
                assert!(seen.insert(id), "dup within 10k: {id}");
            }
        }
    }

    #[test]
    fn real_ids_match_their_family() {
        assert!(CwGuid::parse(REAL_A).unwrap().matches(Family::A));
        assert!(CwGuid::parse(REAL_B).unwrap().matches(Family::B));
    }
}

