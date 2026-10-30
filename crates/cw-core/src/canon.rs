//! Canonical record rendering — the Rust half of the port contract.
//!
//! Mirrors `the tooling scripts` `enc_cell` EXACTLY; the golden tests
//! compare `records_sha` / first-record line against the frozen oracle
//! (`tests\golden\dbf\<corpus>\oracle.json`).
//!
//! Record line: `"{flag:02x}|"` + cells joined `|`, where
//! - C: latin-1, right-strip spaces
//! - N/D: strip spaces both ends (token as-is; NO float formatting anywhere)
//! - L: raw char(s)
//! - M: memo payload; `\r` dropped, `\n` -> `\\n`, ctrl<32 -> `\\xNN`

use crate::dbf::Table;

/// Latin-1: every byte becomes the same-numbered char. Lossless for CW text.
pub fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

fn memo_canon(payload: &[u8]) -> String {
    let mut out = String::with_capacity(payload.len());
    for &b in payload {
        match b {
            b'\r' => {}
            b'\n' => out.push_str("\\n"),
            0..=31 => out.push_str(&format!("\\x{b:02x}")),
            _ => out.push(b as char),
        }
    }
    out
}

/// Canonical rendering of record `r`. `fpt` resolves M cells; `None` renders
/// them empty (used by synthetic tables without sidecars).
pub fn record_line(table: &Table, r: usize, fpt: Option<&crate::fpt::Fpt>) -> String {
    let rec = &table.records[r];
    let offs = table.offsets();
    let mut cells = Vec::with_capacity(table.fields.len());
    for (i, f) in table.fields.iter().enumerate() {
        let raw = &rec[offs[i]..offs[i] + f.len as usize];
        cells.push(match f.kind {
            b'C' => {
                let s = latin1(raw);
                s.trim_end_matches(' ').to_string()
            }
            b'N' | b'D' => {
                let s = latin1(raw);
                s.trim().to_string()
            }
            b'L' => latin1(raw),
            b'M' => match fpt.and_then(|fp| fp.memo(raw)) {
                Some(payload) => memo_canon(payload),
                None => String::new(),
            },
            other => format!("<unk:{}>", other as char),
        });
    }
    format!("{:02x}|{}", rec[0], cells.join("|"))
}

/// SHA-256 (upper hex) over the `\n`-joined canonical lines — the exact
/// string the Python freezer hashes (`"\n".join(lines)`, no trailing nl).
pub fn records_sha(table: &Table, fpt: Option<&crate::fpt::Fpt>) -> String {
    let mut joined = String::new();
    for r in 0..table.count() {
        if r > 0 {
            joined.push('\n');
        }
        joined.push_str(&record_line(table, r, fpt));
    }
    crate::sha256::sha256_hex(joined.as_bytes())
}

