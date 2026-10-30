//! CaseView placeholder expression language — lexer, parser, evaluator (F8).
//!
//! Every CaseView placeholder is a **reference**, never a `{{…}}` sentinel: the
//! `Form/Strings` pool stores the expression text itself, and the cell's
//! `Layout` trailer selects it ([`crate::layout::POOL_INDEX_BASE`], see the
//! `Strings`-pool binding law in [`crate::layout`]). This module gives
//! that text a grammar so a placeholder can be parsed, checked and — where the
//! operands are known — evaluated offline.
//!
//! ## Provenance of the grammar
//!
//! The token set is not inferred from the corpus alone; it is the set the
//! product itself *emits*. `x64cv32.dll` builds every computed-cell formula by
//! `sprintf`-ing a fixed template, and the vendor's expression builder
//! (source anchor `src\cv32\cvbrowse.cpp` / `CELLEDIT.CPP`) has one
//! arm per *function kind*, selected by a small integer discriminator:
//!
//! | kind | emitted template (verbatim) |
//! |---|---|
//! | 2 | `desc("%s"%s)` · `desc(entity("%s","%s")%s)` |
//! | 3 | `%sls("%s%s%s","%s")` · `%sls("%s%s%s", entity("%s","%s"))` |
//! | 4 | `%smap("%s%s%s", "%s")` · `%smap("%s%s%s", entity("%s","%s"))` |
//! | 5 | `lsdesc("%s"%s)` |
//! | 6 | `mapdesc("%s"%s)` |
//! | 7 | `%sclass("%s%s", "%s")` · `%sclass("%s%s", entity("%s","%s"))` |
//! | 8 | `%sfield("%s","%s","%s")` |
//! | 9 | `%sgrp("%s","%s%s%s","%s")` · …`entity(...)` |
//! | 10 | `grpdesc("%s","%s"%s)` · `grpdesc("%s",entity("%s","%s")%s)` |
//! | 11 | *(the user's literal text, passed through unchanged)* |
//! | else | `%sact("%s%s","%s")` · `%sact("%s%s",entity("%s","%s"))` |
//!
//! The leading `%s` of kinds 3/4/7/8/9 and the trailing `%s` of kinds 2/5/6/10
//! are the `:sub` / `:par` qualifier suffix the same function appends when the
//! cell carries `sub_no` (`0x178`) or paragraph scope (`0x17a`), copied from the
//! literals `":sub"` / `":par"`. So the qualifier is *part of the argument
//! string*, not a separate token — consistent with the corpus, where the only
//! qualified form observed is `act('%s:par:usesign',rawent(`, i.e. the qualifier
//! rides inside the first argument.
//!
//! ## Period keys
//!
//! `act()`'s first argument is a **period key**. The canonicaliser in
//! `x64cv32.dll` splits the key at its first
//! digit run into a *prefix* and *suffix*, then case-folds the prefix through a
//! two-stage alias map. The map, transcribed verbatim from the dispatch
//! (`_mbsicmp` chains) is:
//!
//! | prefix | → canonical | note |
//! |---|---|---|
//! | `am` | `disc` | adjustments, disclosure bucket |
//! | `aj` | `an` | AJEs, unadjusted |
//! | `ajc` | `ac` | AJEs, calculated |
//! | `aje` | `ae` | AJEs, eliminated |
//! | `ajf` | `af` | AJEs, factual |
//! | `ajr` | `ar` | AJEs, reclassifying |
//! | `ajs` | `as` | AJEs, … (S) |
//! | `aju` | `au` | AJEs, … (U) |
//! | `ay` | `yr0` | current year |
//! | `ayx` | `fx` | current year, FX |
//! | `b` | `bb:per` | *(only when a digit run follows)* |
//! | `bm` | `bb:disc` | |
//! | `by` | `bb` | |
//! | `byx` | `bb:fx` | |
//! | `cp` | `per` | *(only when a digit run follows)* |
//! | `p` | `yr1:per` | *(only when a digit run follows)* |
//! | `pm` | `yr1:disc` | |
//! | `py` | `yr1` | no digit run → plain `yr1` |
//! | `pyx` | `yr1:fx` | |
//!
//! Everything else is left byte-identical (the copy-out arm of the
//! canonicaliser).
//! The digit run is *preserved*: the canonicaliser rewrites only the prefix.
//! `cpb:yr0` from the corpus is therefore already canonical (`cp` + `b:yr0`
//! does not match `cp` because the next character after `cp` is `b`, not a
//! digit — the `cp` arm is digit-gated), which is what makes `map("cpb:yr0",
//! MAP_NO)` a *balance-column* key rather than a period key.
//!
//! ## Function set
//!
//! Two tiers, and the difference matters for what this crate may claim:
//!
//! * **In-image** (cv32/cw42 emit them and, for the pure ones, the semantics are
//!   ordinary arithmetic/string work): `iif`, `case`, `trim`, `ltrim`, `upper`,
//!   `lower`, `substr`, `left`, `right`, `len`, `str`, `val`, `round`, `abs`,
//!   `mkdate`, `sdate`, `dtos`, and the arithmetic/comparison/boolean operators.
//! * **External** (declared but resolved by `x64CODEBASE.dll`'s `expr4*` family,
//!   which is not staged): `field`, `map`, `act`, `ls`, `class`, `grp`,
//!   `grpdesc`, `mapdesc`, `lsdesc`, `desc`, `entity`, `cvdata`, `docid`,
//!   `dmdsec`/`dmdsec`-family, `rc`, `r`, `prgdir`, `celltable`, `cellrow`,
//!   `rowcount`, `docmode`, `link`, `path`.
//!
//! The second tier is *parsed* here — the grammar is fully known — but its
//! **values** come from a caller-supplied [`resolver::Resolver`]. There is no
//! fabricated fallback: an unresolved call yields [`Value::Unknown`], and an
//! evaluation that needs an unknown operand yields [`EvalError::UnknownOperand`]
//! rather than a guess. That is the honest boundary: the syntax is established, the
//! data plane is not.
//!
//! ## The data tier, now partly established (`x64CODEBASE.dll`)
//!
//! The `expr4*` kernel's *shapes* are now transcribed, which is what lets this
//! module compute two things it previously only parsed:
//!
//! * **`expr4parse`** is a one-line forward with the same arguments plus three
//!   zero fields — so the parser is a single recursive-descent routine, and
//!   parse results are interned into a per-context table (at context offset
//!   `0x476`) with a **depth cap of 0x1f (31)** nested expressions; exceeding
//!   it raises `e4describe_ansi(…, 0xfffffe48, …)` and returns null.
//! * **`expr4true`** forwards to the truth/value test.
//! * **`f4double`** reads a numeric field as
//!   `c4atod(bytes, field_width)`, and **`c4atod`** is
//!   `_atof_l(bytes, locale)` on at most **0x31 (49)** bytes. So the text→number
//!   rule is C `atof` — leading whitespace skipped, longest valid prefix taken,
//!   `0.0` on failure — **not** a strict parse and **not** a `NaN`.
//! * The error channel is `e4describe_ansi`, which formats a
//!   negative status (`0xfffffe48`, `0xfffffe66`, `0xfffffc4a`, …) into text and,
//!   when a window handle is present, raises a message box.
//!
//! What remains *not* established is the per-code mapping of those negative
//! statuses to wording, and the function/field resolution tables (`expr4CVinit`
//! installs callbacks, but the tables themselves are built by the host). This
//! module therefore keeps its own typed errors, and it now uses the **atof**
//! rule for numeric coercion because that rule *is* established.
//!
//! ## Error semantics (established / open)
//!
//! **Established:** the parser is CodeBase's (`expr4parse`, imported from
//! `x64CODEBASE.dll` by both `x64cw42` and `x64cv32`); cv32 supplies CV-specific
//! resolver callbacks (`expr4CVinit`, `expr4CV`, `expr4CVattributes`,
//! `expr4CVfree`) and a pre-parse hook that pre-fills every argument position
//! with `'.'` to the field's declared width (`src\cv32\DB.CPP`) — i.e. arguments
//! are **padded to field width before
//! parsing**, which is why `map('.........','%s')` and `act('..','','.........','%s')`
//! appear with literal dot-runs in the product's own templates.
//!
//! **Open:** the exact diagnostic wording, error offsets and recovery
//! behaviour of CodeBase's parser. No staged artifact contains the
//! `expr4*` bodies, so this module reports its *own* typed errors and refuses to
//! claim they are the vendor's.

use std::fmt;

pub mod resolver;

pub use resolver::{Resolver, UnknownResolver};

/// A token of the placeholder language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    /// Numeric literal, kept as written (CaseView numbers are not IEEE-strict
    /// until an operation forces it).
    Num(String),
    /// `"…"` — the ordinary string form.
    Str(String),
    /// `'…'` — the alternate quote form the product itself emits in
    /// `act('..','','.........','%s')`. Equivalent to [`Tok::Str`].
    StrS(String),
    /// An identifier: a field name (`ac_no`, `CLP13`) or a bare word.
    Ident(String),
    /// A cell reference written with an explicit `->`: `AM->SCHEDULE`.
    /// The lexer splits this into `Ident("AM")`, `Arrow`, `Ident("SCHEDULE")`.
    Arrow,
    LParen,
    RParen,
    Comma,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
    /// `&` — boolean AND (also the string-concatenation operator; see
    /// [`Ast::Bin`] and the `+`/`&` note in the module docs).
    Amp,
    /// `|` — boolean OR.
    Pipe,
    /// `!` — boolean NOT.
    Bang,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// `.` inside an identifier path (`AM.SCHEDULE`, `EXP.B6`).
    Dot,
    Colon,
    /// `::` — the absolute/global reference prefix, used as `::EXP.B6` in the
    /// pool. Distinct from a single `:` (which only appears inside period-key
    /// *strings*, never as an operator).
    DColon,
    Eof,
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tok::Num(s) => write!(f, "number `{s}`"),
            Tok::Str(s) => write!(f, "string {s:?}"),
            Tok::StrS(s) => write!(f, "string '{}'", s),
            Tok::Ident(s) => write!(f, "identifier `{s}`"),
            Tok::Arrow => write!(f, "`->`"),
            Tok::LParen => write!(f, "`(`"),
            Tok::RParen => write!(f, "`)`"),
            Tok::Comma => write!(f, "`,`"),
            Tok::Plus => write!(f, "`+`"),
            Tok::Minus => write!(f, "`-`"),
            Tok::Star => write!(f, "`*`"),
            Tok::Slash => write!(f, "`/`"),
            Tok::Percent => write!(f, "`%`"),
            Tok::Caret => write!(f, "`^`"),
            Tok::Amp => write!(f, "`&`"),
            Tok::Pipe => write!(f, "`|`"),
            Tok::Bang => write!(f, "`!`"),
            Tok::Eq => write!(f, "`=`"),
            Tok::Ne => write!(f, "`<>`"),
            Tok::Lt => write!(f, "`<`"),
            Tok::Le => write!(f, "`<=`"),
            Tok::Gt => write!(f, "`>`"),
            Tok::Ge => write!(f, "`>=`"),
            Tok::Dot => write!(f, "`.`"),
            Tok::Colon => write!(f, "`:`"),
            Tok::DColon => write!(f, "`::`"),
            Tok::Eof => write!(f, "end of expression"),
        }
    }
}

/// Why a span could not be lexed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    /// Byte offset of the offending byte.
    pub at: usize,
    /// The byte itself (or `None` at end of input).
    pub byte: Option<u8>,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.byte {
            Some(b) => write!(f, "unexpected byte 0x{b:02X} at offset {}", self.at),
            None => write!(f, "unexpected end of expression at offset {}", self.at),
        }
    }
}

/// Why a token stream could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The lexer rejected a byte.
    Lex(LexError),
    /// A token appeared where the grammar does not allow it.
    Unexpected { found: String, at: usize },
    /// The expression ended while a construct was still open.
    UnexpectedEof { expected: String },
    /// A call was written with too many arguments for its arity.
    Arity { function: String, max: usize, got: usize },
    /// The expression was empty (only whitespace).
    Empty,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Lex(e) => write!(f, "{e}"),
            ParseError::Unexpected { found, at } => {
                write!(f, "unexpected {found} at offset {at}")
            }
            ParseError::UnexpectedEof { expected } => {
                write!(f, "expression ended; expected {expected}")
            }
            ParseError::Arity { function, max, got } => write!(
                f,
                "`{function}` takes at most {max} argument(s), {got} given"
            ),
            ParseError::Empty => f.write_str("empty expression"),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<LexError> for ParseError {
    fn from(e: LexError) -> Self {
        ParseError::Lex(e)
    }
}

/// One node of a parsed placeholder expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ast {
    /// A numeric literal, exactly as written.
    Num(String),
    /// A string literal (either quote form), already unescaped.
    Str(String),
    /// A bare name: a field (`ac_no`), a cell (`CLP13`), or a scope qualifier
    /// (`CV`, `AM`, `SH`, `MP`) when it precedes `->`/`.`.
    Name(String),
    /// `A->B` or `A.B` — a field reference through a table alias.
    Field(Box<Ast>, String),
    /// A cell reference by number: `1#`, `-13#`, `1#:par`. The `#` sigil is the
    /// product's cell-number marker (`rc(1#)`, `rc(-13#)` in the pool).
    CellRef { index: String, qual: Option<String> },
    /// A `::`-qualified absolute reference, e.g. `::EXP.B6`.
    AbsRef(Box<Ast>),
    /// `expr:qual` — the `:sub` / `:par` scope qualifier the cv32 builder appends
    /// outside the quoted argument (`desc("%s"%s)` with `%s` = `:par`).
    Qualified(Box<Ast>, String),
    /// A newline-separated statement list. A CaseView cell's action body stores
    /// several `Action(...)` statements in one pool entry (pool entry 9 of
    /// `EXPO0IS0`: `Action("PathDialog",…)\nAction("Recalculate")`).
    Seq(Vec<Ast>),
    /// `name(arg, …)`.
    Call { name: String, args: Vec<Ast> },
    /// A prefix operator: `-`, `+`, `!`.
    Unary { op: char, arg: Box<Ast> },
    /// An infix operator.
    Bin {
        op: BinOp,
        left: Box<Ast>,
        right: Box<Ast>,
    },
    /// A parenthesised expression (kept for exact re-serialisation).
    Paren(Box<Ast>),
}

/// The infix operators of the language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    /// `&` — CaseView's overloaded AND/concat operator: boolean when both
    /// sides are truth values, concatenation otherwise.
    And,
    /// `|` — OR.
    Or,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinOp {
    /// Binding power, higher binds tighter. CaseView's precedence is the
    /// familiar one: comparison < `&`/`|` < `+`/`-` < `*`//`%` < `^`, with `!`
    /// and unary minus above everything. (The builder never emits mixed
    /// precedence without parentheses except for `&` against comparisons and
    /// `+` chains, both of which this table gets right.)
    fn binding(self) -> u8 {
        match self {
            BinOp::Or => 2,
            BinOp::And => 3,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 4,
            BinOp::Add | BinOp::Sub => 5,
            BinOp::Mul | BinOp::Div | BinOp::Mod => 6,
            BinOp::Pow => 7,
        }
    }

    fn from_tok(t: &Tok) -> Option<Self> {
        Some(match t {
            Tok::Plus => BinOp::Add,
            Tok::Minus => BinOp::Sub,
            Tok::Star => BinOp::Mul,
            Tok::Slash => BinOp::Div,
            Tok::Percent => BinOp::Mod,
            Tok::Caret => BinOp::Pow,
            Tok::Amp => BinOp::And,
            Tok::Pipe => BinOp::Or,
            Tok::Eq => BinOp::Eq,
            Tok::Ne => BinOp::Ne,
            Tok::Lt => BinOp::Lt,
            Tok::Le => BinOp::Le,
            Tok::Gt => BinOp::Gt,
            Tok::Ge => BinOp::Ge,
            _ => return None,
        })
    }
}

/// Tokenise a placeholder expression.
pub fn lex(src: &str) -> Result<Vec<Tok>, LexError> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        match c {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            b')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            b',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            b'+' => {
                out.push(Tok::Plus);
                i += 1;
            }
            b'-' => {
                if b.get(i + 1) == Some(&b'>') {
                    out.push(Tok::Arrow);
                    i += 2;
                } else {
                    out.push(Tok::Minus);
                    i += 1;
                }
            }
            b'*' => {
                out.push(Tok::Star);
                i += 1;
            }
            b'/' => {
                out.push(Tok::Slash);
                i += 1;
            }
            b'%' => {
                out.push(Tok::Percent);
                i += 1;
            }
            b'^' => {
                out.push(Tok::Caret);
                i += 1;
            }
            b'&' => {
                // `&&` is accepted as a synonym: the product emits both forms
                // (`&` in the index expressions, `&&` in `EXPO0IS0` pool entry
                // 95/159/169).
                out.push(Tok::Amp);
                i += if b.get(i + 1) == Some(&b'&') { 2 } else { 1 };
            }
            b'|' => {
                out.push(Tok::Pipe);
                i += if b.get(i + 1) == Some(&b'|') { 2 } else { 1 };
            }
            b'!' => {
                // `!=` is not CaseView spelling (`<>` is), but accept it.
                if b.get(i + 1) == Some(&b'=') {
                    out.push(Tok::Ne);
                    i += 2;
                } else {
                    out.push(Tok::Bang);
                    i += 1;
                }
            }
            b'=' => {
                if b.get(i + 1) == Some(&b'=') {
                    out.push(Tok::Eq);
                    i += 2;
                } else {
                    out.push(Tok::Eq);
                    i += 1;
                }
            }
            b'<' => {
                match b.get(i + 1) {
                    Some(b'=') => {
                        out.push(Tok::Le);
                        i += 2;
                    }
                    Some(b'>') => {
                        out.push(Tok::Ne);
                        i += 2;
                    }
                    _ => {
                        out.push(Tok::Lt);
                        i += 1;
                    }
                }
            }
            b'>' => {
                if b.get(i + 1) == Some(&b'=') {
                    out.push(Tok::Ge);
                    i += 2;
                } else {
                    out.push(Tok::Gt);
                    i += 1;
                }
            }
            b'.' => {
                out.push(Tok::Dot);
                i += 1;
            }
            b':' => {
                if b.get(i + 1) == Some(&b':') {
                    out.push(Tok::DColon);
                    i += 2;
                } else {
                    out.push(Tok::Colon);
                    i += 1;
                }
            }
            b'"' => {
                let (s, next) = read_quoted(b, i, b'"')?;
                out.push(Tok::Str(s));
                i = next;
            }
            b'\'' => {
                let (s, next) = read_quoted(b, i, b'\'')?;
                out.push(Tok::StrS(s));
                i = next;
            }
            c if c.is_dec_ascii() || c == b'$' => {
                let start = i;
                while i < b.len()
                    && (b[i].is_dec_ascii() || b[i] == b'$' || b[i] == b'.' || b[i] == b'#')
                {
                    // A `.` continues a number only when a digit follows, so
                    // `EXP.B6` lexes as ident/dot/ident while `1.0` is a number.
                    if b[i] == b'.' {
                        match b.get(i + 1) {
                            Some(d) if d.is_ascii_digit() => {}
                            _ => break,
                        }
                    }
                    i += 1;
                }
                out.push(Tok::Num(src[start..i].to_string()));
            }
            c if c.is_ident_start() => {
                let start = i;
                while i < b.len() && b[i].is_ident_continue() {
                    i += 1;
                }
                out.push(Tok::Ident(src[start..i].to_string()));
            }
            other => return Err(LexError { at: i, byte: Some(other) }),
        }
    }
    out.push(Tok::Eof);
    Ok(out)
}

trait Bytes {
    fn is_dec_ascii(&self) -> bool;
    fn is_ident_start(&self) -> bool;
    fn is_ident_continue(&self) -> bool;
}

impl Bytes for u8 {
    fn is_dec_ascii(&self) -> bool {
        self.is_ascii_digit()
    }
    fn is_ident_start(&self) -> bool {
        self.is_ascii_alphabetic() || *self == b'_'
    }
    fn is_ident_continue(&self) -> bool {
        // `#` is the record/cell sigil: `rc(1#)`, `rc(-13#)` in the pool.
        self.is_ascii_alphanumeric() || *self == b'_' || *self == b'#'
    }
}

fn read_quoted(b: &[u8], open: usize, q: u8) -> Result<(String, usize), LexError> {
    let mut i = open + 1;
    let mut s = String::new();
    while i < b.len() {
        let c = b[i];
        if c == q {
            // Doubled quote is the only escape: `""` inside a `"` literal. A
            // backslash is *literal* — the pool's `PATH+"\scripts\AAR.scp"` and
            // `TB1.B3 + "\data\trs\clients\"` both rely on that, and treating
            // `\"` as an escape would swallow the closing quote of the second.
            if b.get(i + 1) == Some(&q) {
                s.push(q as char);
                i += 2;
                continue;
            }
            return Ok((s, i + 1));
        }
        // Latin-1 passthrough: pool text is not UTF-8.
        s.push(c as char);
        i += 1;
    }
    Err(LexError { at: open, byte: None })
}

/// Parse a placeholder expression.
pub fn parse(src: &str) -> Result<Ast, ParseError> {
    let toks = lex(src)?;
    let mut p = Parser { toks, i: 0 };
    if matches!(p.peek(), Tok::Eof) {
        return Err(ParseError::Empty);
    }
    let first = p.expr(0)?;
    let mut stmts = vec![first];
    loop {
        match p.peek() {
            Tok::Eof => break,
            Tok::Comma => {
                // A bare comma at statement level continues the list: the
                // product's action bodies use it as a separator too.
                p.bump();
                stmts.push(p.expr(0)?);
            }
            _ => {
                return Err(ParseError::Unexpected {
                    found: p.peek().to_string(),
                    at: p.offset(),
                })
            }
        }
    }
    Ok(if stmts.len() == 1 {
        stmts.pop().unwrap()
    } else {
        Ast::Seq(stmts)
    })
}

/// Parse a newline-separated statement list (a cell's action body).
///
/// The pools store these as one entry, so a caller that wants each statement
/// separately splits on newlines and parses each. This is the convenience form.
pub fn parse_statements(src: &str) -> Result<Vec<Ast>, ParseError> {
    let mut out = Vec::new();
    for line in src.split('\n') {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        out.push(parse(t)?);
    }
    if out.is_empty() {
        return Err(ParseError::Empty);
    }
    Ok(out)
}

struct Parser {
    toks: Vec<Tok>,
    i: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        self.toks.get(self.i).unwrap_or(&Tok::Eof)
    }

    fn bump(&mut self) -> Tok {
        let t = self.toks.get(self.i).cloned().unwrap_or(Tok::Eof);
        self.i += 1;
        t
    }

    /// Byte offset is not tracked per token (the lexer discards spans); report
    /// the token index as the position, which is what the diagnostics use.
    fn offset(&self) -> usize {
        self.i
    }

    fn expr(&mut self, min_bp: u8) -> Result<Ast, ParseError> {
        let mut lhs = self.unary()?;
        loop {
            let Some(op) = BinOp::from_tok(self.peek()) else {
                break;
            };
            let bp = op.binding();
            if bp < min_bp {
                break;
            }
            self.bump();
            // `^` is right-associative; everything else is left-associative.
            let next_min = if op == BinOp::Pow { bp } else { bp + 1 };
            let rhs = self.expr(next_min)?;
            lhs = Ast::Bin {
                op,
                left: Box::new(lhs),
                right: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Ast, ParseError> {
        match self.peek().clone() {
            Tok::Minus => {
                self.bump();
                Ok(Ast::Unary {
                    op: '-',
                    arg: Box::new(self.unary()?),
                })
            }
            Tok::Plus => {
                self.bump();
                Ok(Ast::Unary {
                    op: '+',
                    arg: Box::new(self.unary()?),
                })
            }
            Tok::Bang => {
                self.bump();
                Ok(Ast::Unary {
                    op: '!',
                    arg: Box::new(self.unary()?),
                })
            }
            _ => self.postfix(),
        }
    }

    fn postfix(&mut self) -> Result<Ast, ParseError> {
        let mut node = self.primary()?;
        loop {
            match self.peek() {
                Tok::Arrow | Tok::Dot => {
                    self.bump();
                    match self.bump() {
                        Tok::Ident(name) => {
                            node = Ast::Field(Box::new(node), name);
                        }
                        other => {
                            return Err(ParseError::Unexpected {
                                found: other.to_string(),
                                at: self.offset(),
                            })
                        }
                    }
                }
                Tok::Colon => {
                    // Only a *qualifier* here: `:sub`, `:par`, `:usesign`.
                    self.bump();
                    match self.bump() {
                        Tok::Ident(q) => {
                            node = Ast::Qualified(Box::new(node), q);
                        }
                        other => {
                            return Err(ParseError::Unexpected {
                                found: other.to_string(),
                                at: self.offset(),
                            })
                        }
                    }
                }
                _ => return Ok(node),
            }
        }
    }

    fn primary(&mut self) -> Result<Ast, ParseError> {
        match self.bump() {
            Tok::DColon => Ok(Ast::AbsRef(Box::new(self.postfix()?))),
            Tok::Num(s) => {
                // `1#` / `-13#` is a cell reference, not a number. The lexer
                // keeps the `#` inside the numeric token.
                if let Some(idx) = s.strip_suffix('#') {
                    let mut qual = None;
                    if matches!(self.peek(), Tok::Colon) {
                        self.bump();
                        if let Tok::Ident(q) = self.bump() {
                            qual = Some(q);
                        }
                    }
                    return Ok(Ast::CellRef {
                        index: idx.to_string(),
                        qual,
                    });
                }
                Ok(Ast::Num(s))
            }
            Tok::Str(s) | Tok::StrS(s) => Ok(Ast::Str(s)),
            Tok::Ident(name) => {
                if matches!(self.peek(), Tok::LParen) {
                    self.bump();
                    let mut args = Vec::new();
                    if matches!(self.peek(), Tok::RParen) {
                        self.bump();
                    } else {
                        loop {
                            args.push(self.expr(0)?);
                            match self.bump() {
                                Tok::Comma => continue,
                                Tok::RParen => break,
                                other => {
                                    return Err(ParseError::Unexpected {
                                        found: other.to_string(),
                                        at: self.offset(),
                                    })
                                }
                            }
                        }
                    }
                    Ok(Ast::Call { name, args })
                } else {
                    Ok(Ast::Name(name))
                }
            }
            Tok::LParen => {
                let inner = self.expr(0)?;
                match self.bump() {
                    Tok::RParen => Ok(Ast::Paren(Box::new(inner))),
                    other => Err(ParseError::Unexpected {
                        found: other.to_string(),
                        at: self.offset(),
                    }),
                }
            }
            Tok::Eof => Err(ParseError::UnexpectedEof {
                expected: "an operand".to_string(),
            }),
            other => Err(ParseError::Unexpected {
                found: other.to_string(),
                at: self.offset(),
            }),
        }
    }
}

/// A computed value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A number. Kept as `f64`; CaseView's own arithmetic is `double` for
    /// computed cells (`expr4double` is an `x64CODEBASE` export used by the
    /// field family).
    Num(f64),
    /// Text.
    Str(String),
    /// A date, as the Julian day number CaseView uses everywhere (`PERTOJ`,
    /// `mkdate`, `sdate`, `DTOS` are all Julian-day based in the corpus's own
    /// expressions).
    Date(i64),
    /// The value could not be resolved offline. This is not an error at the
    /// AST level — it is the honest result for a data reference.
    Unknown,
    /// The empty/missing value (`""`, blank cell).
    Empty,
}

impl Value {
    /// CaseView truth: blank is false, zero is false, non-zero is true,
    /// non-empty text is true. `"0"` parses to a number and is therefore false.
    pub fn truth(&self) -> Option<bool> {
        Some(match self {
            Value::Num(n) => *n != 0.0,
            Value::Str(s) => {
                let t = s.trim();
                // `expr4true` treats a cell as true when its numeric reading is
                // non-zero; the reading is `atof`, so non-numeric text is 0.0 and
                // therefore FALSE. This is the established rule for the value test.
                if t.is_empty() {
                    false
                } else {
                    atof(t) != 0.0
                }
            }
            Value::Date(d) => *d != 0,
            Value::Empty => false,
            Value::Unknown => return None,
        })
    }

    /// Numeric coercion.
    ///
    /// Text goes through [`atof`], the established rule (`c4atod` is `_atof_l`), and
    /// therefore **never fails**: unparseable text is `0.0`. That is a real
    /// semantic difference from a strict parse — `"12abc"` is `12.0`, and
    /// `"abc"` is `0.0`, exactly as the product behaves.
    pub fn num(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            Value::Date(d) => Some(*d as f64),
            Value::Str(s) => Some(atof(s)),
            Value::Empty => Some(0.0),
            Value::Unknown => None,
        }
    }

    /// Text coercion.
    pub fn text(&self) -> Option<String> {
        Some(match self {
            Value::Num(n) => fmt_num(*n),
            Value::Date(d) => d.to_string(),
            Value::Str(s) => s.clone(),
            Value::Empty => String::new(),
            Value::Unknown => return None,
        })
    }
}

/// Render a number the way a text cell would: integral values without a
/// fractional part, everything else with the shortest round-trip form.
/// C `atof` on a Rust string — the rule `c4atod` (`X64CODEBASE.dll`) uses.
///
/// Semantics, in order: skip leading whitespace; accept an optional sign; accept
/// digits and at most one decimal point; accept an exponent (`e`/`E`, optional
/// sign, digits) only when at least one mantissa digit was seen; stop at the
/// first byte that cannot continue the number and evaluate what was consumed.
/// No digits consumed ⇒ `0.0`.
///
/// Two vendor details are preserved because they are observable: the input is
/// truncated to 49 bytes (`c4atod` clamps the length to `0x31`), and the result is
/// never `NaN`/`inf` from bad text.
pub fn atof(src: &str) -> f64 {
    let bytes = src.as_bytes();
    let limit = bytes.len().min(49);
    let mut i = 0usize;
    while i < limit && (bytes[i] as char).is_ascii_whitespace() {
        i += 1;
    }
    let start = i;
    if i < limit && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let mut digits = 0usize;
    while i < limit && bytes[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if i < limit && bytes[i] == b'.' {
        i += 1;
        while i < limit && bytes[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return 0.0;
    }
    if i < limit && (bytes[i] == b'e' || bytes[i] == b'E') {
        let mut j = i + 1;
        if j < limit && (bytes[j] == b'+' || bytes[j] == b'-') {
            j += 1;
        }
        let mut edigits = 0usize;
        while j < limit && bytes[j].is_ascii_digit() {
            j += 1;
            edigits += 1;
        }
        if edigits > 0 {
            i = j;
        }
    }
    src[start..i].parse::<f64>().unwrap_or(0.0)
}

pub fn fmt_num(n: f64) -> String {
    if n == n.trunc() && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// Why evaluation could not produce a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalError {
    /// An operand depended on data that is not available offline.
    UnknownOperand { at: String },
    /// A function name is not in the language's vocabulary.
    UnknownFunction { name: String },
    /// The evaluator reached a call it knows the syntax of but cannot compute
    /// (every `x64CODEBASE`-resolved function).
    NotComputable { name: String },
    /// Division by zero.
    DivideByZero,
    /// A string function got a non-string, or a numeric one a non-number.
    TypeMismatch { function: String },
    /// A call had the wrong number of arguments.
    Arity { function: String, want: usize, got: usize },
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvalError::UnknownOperand { at } => write!(f, "unresolved operand in `{at}`"),
            EvalError::UnknownFunction { name } => write!(f, "unknown function `{name}`"),
            EvalError::NotComputable { name } => write!(
                f,
                "`{name}` is resolved by the data layer (expr4), not computable offline"
            ),
            EvalError::DivideByZero => f.write_str("division by zero"),
            EvalError::TypeMismatch { function } => {
                write!(f, "argument type mismatch in `{function}`")
            }
            EvalError::Arity { function, want, got } => write!(
                f,
                "`{function}` takes {want} argument(s), {got} given"
            ),
        }
    }
}

impl std::error::Error for EvalError {}

/// The canonical period-key alias table, as the vendor's cell editor matches
/// it (longest-prefix wins, case-insensitive).
///
/// Each row is `(prefix, canonical, digit_gated)`. A `digit_gated` row only
/// applies when the prefix is followed by a digit run, which is exactly what
/// separates `cp`+`b:yr0` (not `cp`) from `cp`+`12`.
pub const PERIOD_ALIASES: &[(&str, &str, bool)] = &[
    ("am", "disc", false),
    ("aj", "an", false),
    ("ajc", "ac", false),
    ("aje", "ae", false),
    ("ajf", "af", false),
    ("ajr", "ar", false),
    ("ajs", "as", false),
    ("aju", "au", false),
    ("ay", "yr0", false),
    ("ayx", "fx", false),
    ("b", "bb:per", true),
    ("bm", "bb:disc", false),
    ("by", "bb", false),
    ("byx", "bb:fx", false),
    ("cp", "per", true),
    ("p", "yr1:per", true),
    ("pm", "yr1:disc", false),
    ("py", "yr1", false),
    ("pyx", "yr1:fx", false),
];

/// Canonicalise a period key by rewriting its alphabetic prefix.
///
/// The key is split at its **first digit run**; the prefix (everything before
/// it) is case-folded through [`PERIOD_ALIASES`] and the digit run plus any
/// trailing suffix is preserved verbatim. This is the whole of the
/// canonicaliser's observable behaviour, including its fall-through: an
/// unrecognised prefix is copied out unchanged.
///
/// Longest-prefix-first matching is required: `pyx` must beat `py`, and `ajc`
/// must beat `aj`, exactly as the nested `_mbsicmp` chains do.
pub fn canonical_period_key(key: &str) -> String {
    let b = key.as_bytes();
    // Find the split: first position where a digit begins a run.
    let mut split = None;
    for (i, &c) in b.iter().enumerate() {
        if c.is_ascii_digit() {
            // A digit at position 0 means there is no alphabetic prefix.
            if i == 0 {
                return key.to_string();
            }
            split = Some(i);
            break;
        }
    }
    let (prefix, rest) = match split {
        Some(i) => (&key[..i], &key[i..]),
        None => (key, ""),
    };
    let low = prefix.to_ascii_lowercase();
    let mut best: Option<(&str, &str)> = None;
    for &(from, to, gated) in PERIOD_ALIASES {
        if low == from && (!gated || !rest.is_empty()) {
            // Exact whole-prefix match wins; longer prefixes were compared
            // first only by construction of the table, so take the longest.
            match best {
                Some((bf, _)) if bf.len() >= from.len() => {}
                _ => best = Some((from, to)),
            }
        }
    }
    match best {
        Some((_, to)) => format!("{to}{rest}"),
        None => key.to_string(),
    }
}

/// A period key, decomposed into its two independent axes.
///
/// `act("yr0:bu", ac_no)` addresses *year `yr0`* in *bucket `bu`*; `act("an",
/// ac_no)` addresses a *named bucket with no year qualifier*. The two axes are
/// genuinely independent, which is why the canonicaliser above leaves the part
/// after the colon alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeriodKey<'a> {
    /// Canonical year/bucket prefix, e.g. `yr0`, `yr1`, `bb`, `an`, `per`.
    pub root: String,
    /// Sub-key after the `:`, e.g. `bu`, `bc`, `disc`, `fx`, `per`.
    pub sub: Option<&'a str>,
}

impl<'a> PeriodKey<'a> {
    /// Canonicalise and split a period key.
    pub fn parse(key: &'a str) -> PeriodKey<'a> {
        let canon = canonical_period_key(key);
        // Re-borrow the sub-key from the canonical form; it is either a suffix
        // of the input or introduced by an alias, so keep it owned-safe by
        // slicing `key` when possible and falling back to the canonical text.
        match canon.split_once(':') {
            Some((root, sub)) => {
                let sub_in_input = key.split_once(':').map(|(_, s)| s);
                let sub_ref = sub_in_input.filter(|s| *s == sub).or_else(|| {
                    // The alias introduced a `:`; the sub-key is still a suffix
                    // of the canonical string, and the canonical string borrows
                    // nothing from `key`, so we cannot return a `&'a str` for it.
                    let _ = sub;
                    None
                });
                PeriodKey {
                    root: root.to_string(),
                    sub: sub_ref,
                }
            }
            None => PeriodKey {
                root: canon,
                sub: None,
            },
        }
    }

    /// The sub-key as owned text, including the alias-introduced case.
    pub fn sub_owned(&self, key: &str) -> Option<String> {
        if let Some(s) = self.sub {
            return Some(s.to_string());
        }
        let canon = canonical_period_key(key);
        canon
            .split_once(':')
            .map(|(_, s)| s.to_string())
    }
}

/// Evaluate an expression against an optional resolver.
///
/// Pure functions and operators are computed. A call to a data-layer function
/// goes through `resolver`; when the resolver returns `None` the result is
/// [`Value::Unknown`], and any further operation that needs it fails with
/// [`EvalError::UnknownOperand`].
pub fn eval(ast: &Ast, resolver: &dyn Resolver) -> Result<Value, EvalError> {
    match ast {
        Ast::Num(s) => s
            .parse::<f64>()
            .map(Value::Num)
            .map_err(|_| EvalError::TypeMismatch {
                function: s.clone(),
            }),
        Ast::Str(s) => Ok(Value::Str(s.clone())),
        Ast::Name(n) => Ok(resolver.name(n).unwrap_or(Value::Unknown)),
        Ast::Field(base, field) => {
            // `AM->SCHEDULE` names a *table alias*, not a cell: the left side is
            // passed through as text rather than evaluated. Only a computed left
            // side (which the language allows) is evaluated first.
            let b = match &**base {
                Ast::Name(n) => n.clone(),
                other => match eval(other, resolver)? {
                    Value::Str(s) => s,
                    Value::Unknown => return Ok(Value::Unknown),
                    v => v.text().unwrap_or_default(),
                },
            };
            Ok(resolver.field(&b, field).unwrap_or(Value::Unknown))
        }
        Ast::Paren(inner) => eval(inner, resolver),
        Ast::AbsRef(inner) => eval(inner, resolver),
        Ast::Qualified(inner, _) => eval(inner, resolver),
        Ast::Seq(items) => {
            // Statements have no value; a sequence's value is its last.
            let mut last = Value::Empty;
            for it in items {
                last = eval(it, resolver)?;
            }
            Ok(last)
        }
        Ast::CellRef { index, qual } => {
            // A cell reference resolves through the document's cell table, which
            // is exactly the data plane. `rc()` above it is resolved by expr4.
            let name = match qual {
                Some(q) => format!("{index}#:{q}"),
                None => format!("{index}#"),
            };
            Ok(resolver.name(&name).unwrap_or(Value::Unknown))
        }
        Ast::Unary { op, arg } => {
            let v = eval(arg, resolver)?;
            match op {
                '!' => match v.truth() {
                    Some(t) => Ok(Value::Num(if t { 0.0 } else { 1.0 })),
                    None => Ok(Value::Unknown),
                },
                '-' => match v.num() {
                    Some(n) => Ok(Value::Num(-n)),
                    None => Ok(Value::Unknown),
                },
                _ => Ok(v),
            }
        }
        Ast::Bin { op, left, right } => eval_bin(*op, left, right, resolver),
        Ast::Call { name, args } => eval_call(name, args, resolver),
    }
}

fn eval_bin(
    op: BinOp,
    left: &Ast,
    right: &Ast,
    resolver: &dyn Resolver,
) -> Result<Value, EvalError> {
    let l = eval(left, resolver)?;
    // `iif`-style short circuit is not available at the operator level except
    // for `&`/`|`, where CaseView does evaluate both sides (it has no side
    // effects in the language), so both are always evaluated.
    let r = eval(right, resolver)?;
    match op {
        BinOp::And => match (l.truth(), r.truth()) {
            (Some(a), Some(b)) => Ok(Value::Num(if a && b { 1.0 } else { 0.0 })),
            (Some(false), _) | (_, Some(false)) => Ok(Value::Num(0.0)),
            _ => Ok(Value::Unknown),
        },
        BinOp::Or => match (l.truth(), r.truth()) {
            (Some(a), Some(b)) => Ok(Value::Num(if a || b { 1.0 } else { 0.0 })),
            (Some(true), _) | (_, Some(true)) => Ok(Value::Num(1.0)),
            _ => Ok(Value::Unknown),
        },
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            let ord = compare(&l, &r);
            let Some(ord) = ord else {
                return Ok(Value::Unknown);
            };
            let t = match op {
                BinOp::Eq => ord == std::cmp::Ordering::Equal,
                BinOp::Ne => ord != std::cmp::Ordering::Equal,
                BinOp::Lt => ord == std::cmp::Ordering::Less,
                BinOp::Le => ord != std::cmp::Ordering::Greater,
                BinOp::Gt => ord == std::cmp::Ordering::Greater,
                BinOp::Ge => ord != std::cmp::Ordering::Less,
                _ => unreachable!(),
            };
            Ok(Value::Num(if t { 1.0 } else { 0.0 }))
        }
        BinOp::Add => {
            // `+` is the concatenation operator when either side is text.
            match (&l, &r) {
                (Value::Num(a), Value::Num(b)) => Ok(Value::Num(a + b)),
                (Value::Unknown, _) | (_, Value::Unknown) => Ok(Value::Unknown),
                _ => match (l.text(), r.text()) {
                    (Some(a), Some(b)) => Ok(Value::Str(a + &b)),
                    _ => Ok(Value::Unknown),
                },
            }
        }
        BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod | BinOp::Pow => {
            let (Some(a), Some(b)) = (l.num(), r.num()) else {
                return Ok(Value::Unknown);
            };
            Ok(Value::Num(match op {
                BinOp::Sub => a - b,
                BinOp::Mul => a * b,
                BinOp::Div => {
                    if b == 0.0 {
                        return Err(EvalError::DivideByZero);
                    }
                    a / b
                }
                BinOp::Mod => {
                    if b == 0.0 {
                        return Err(EvalError::DivideByZero);
                    }
                    a % b
                }
                BinOp::Pow => a.powf(b),
                _ => unreachable!(),
            }))
        }
    }
}

/// Compare two values with CaseView's coercions: numeric when both sides
/// coerce, else text.
fn compare(l: &Value, r: &Value) -> Option<std::cmp::Ordering> {
    if matches!(l, Value::Unknown) || matches!(r, Value::Unknown) {
        return None;
    }
    if let (Some(a), Some(b)) = (l.num(), r.num()) {
        return a.partial_cmp(&b);
    }
    let (a, b) = (l.text()?, r.text()?);
    Some(a.cmp(&b))
}

fn arity(name: &str, args: &[Ast], want: usize) -> Result<(), EvalError> {
    if args.len() == want {
        Ok(())
    } else {
        Err(EvalError::Arity {
            function: name.to_string(),
            want,
            got: args.len(),
        })
    }
}

/// Functions whose semantics are ordinary and fully determined by their
/// arguments. Anything not listed here is a data reference.
pub const PURE_FUNCTIONS: &[&str] = &[
    "iif", "case", "trim", "ltrim", "rtrim", "upper", "lower", "substr", "left", "right", "len",
    "str", "val", "round", "abs", "mkdate", "sdate", "dtos", "space", "monthstr", "pertoj", "jtop",
    "jtoperm", "min", "max", "int", "mod",
];

/// Every function name the product's own templates or the corpus pools use.
pub const DATA_FUNCTIONS: &[&str] = &[
    "field", "map", "act", "ls", "class", "grp", "grpdesc", "mapdesc", "lsdesc", "desc",
    "entity", "cvdata", "docid", "dmdesc", "dmdsec", "rc", "r", "prgdir", "celltable", "cellrow",
    "rowcount", "docmode", "link", "path", "rawent", "inscope", "isblank", "isplholder",
    "in", "itemprop", "scanstr", "baldesc", "adesc",
    // UI/scripting verbs the pool emits as *data* (a cell's action list), not as
    // arithmetic: `Action("InsertRow",cellTable(),…)`, `link()`.
    "action", "nAction", "link", "cellno",
];

fn eval_call(name: &str, args: &[Ast], resolver: &dyn Resolver) -> Result<Value, EvalError> {
    let lname = name.to_ascii_lowercase();
    match lname.as_str() {
        "iif" => {
            if args.len() != 3 {
                return Err(EvalError::Arity {
                    function: name.to_string(),
                    want: 3,
                    got: args.len(),
                });
            }
            match eval(&args[0], resolver)?.truth() {
                Some(true) => eval(&args[1], resolver),
                Some(false) => eval(&args[2], resolver),
                None => Ok(Value::Unknown),
            }
        }
        "case" => {
            // `case(sel, v1, cond2, v2, …, else)` — the corpus form is
            // `case(rc(-13#)="1", AM->SCHEDULE=R(0), rc(-13#)="2", …)`.
            // CaseView's `case` is a value/result pair chain with a final
            // default; it is *not* a selector switch, so it is evaluated
            // left-to-right returning the first value whose test is true.
            if args.len() < 2 {
                return Err(EvalError::Arity {
                    function: name.to_string(),
                    want: 2,
                    got: args.len(),
                });
            }
            let mut i = 0;
            while i + 1 < args.len() {
                if eval(&args[i], resolver)?.truth() == Some(true) {
                    return eval(&args[i + 1], resolver);
                }
                i += 2;
            }
            if i < args.len() {
                return eval(&args[i], resolver);
            }
            Ok(Value::Empty)
        }
        "trim" | "ltrim" | "rtrim" | "upper" | "lower" => {
            arity(name, args, 1)?;
            let v = eval(&args[0], resolver)?;
            let Some(s) = v.text() else {
                return Ok(Value::Unknown);
            };
            Ok(Value::Str(match lname.as_str() {
                "trim" => s.trim().to_string(),
                "ltrim" => s.trim_start().to_string(),
                "rtrim" => s.trim_end().to_string(),
                "upper" => s.to_uppercase(),
                _ => s.to_lowercase(),
            }))
        }
        "len" => {
            arity(name, args, 1)?;
            let v = eval(&args[0], resolver)?;
            match v.text() {
                Some(s) => Ok(Value::Num(s.chars().count() as f64)),
                None => Ok(Value::Unknown),
            }
        }
        "substr" => {
            if args.len() != 3 {
                return Err(EvalError::Arity {
                    function: name.to_string(),
                    want: 3,
                    got: args.len(),
                });
            }
            let v = eval(&args[0], resolver)?;
            let (Some(start), Some(len)) = (
                eval(&args[1], resolver)?.num(),
                eval(&args[2], resolver)?.num(),
            ) else {
                return Ok(Value::Unknown);
            };
            let Some(s) = v.text() else {
                return Ok(Value::Unknown);
            };
            let chars: Vec<char> = s.chars().collect();
            // CaseView is 1-based and clamps instead of panicking.
            let start = start.max(1.0) as usize - 1;
            let len = len.max(0.0) as usize;
            let out: String = chars.iter().skip(start).take(len).collect();
            Ok(Value::Str(out))
        }
        "left" | "right" => {
            if args.len() != 2 {
                return Err(EvalError::Arity {
                    function: name.to_string(),
                    want: 2,
                    got: args.len(),
                });
            }
            let v = eval(&args[0], resolver)?;
            let Some(n) = eval(&args[1], resolver)?.num() else {
                return Ok(Value::Unknown);
            };
            let Some(s) = v.text() else {
                return Ok(Value::Unknown);
            };
            let n = n.max(0.0) as usize;
            let chars: Vec<char> = s.chars().collect();
            let out: String = if lname == "left" {
                chars.iter().take(n).collect()
            } else {
                let skip = chars.len().saturating_sub(n);
                chars.iter().skip(skip).collect()
            };
            Ok(Value::Str(out))
        }
        "space" => {
            arity(name, args, 1)?;
            match eval(&args[0], resolver)?.num() {
                Some(n) => Ok(Value::Str(" ".repeat(n.max(0.0) as usize))),
                None => Ok(Value::Unknown),
            }
        }
        "str" => {
            if args.is_empty() || args.len() > 2 {
                return Err(EvalError::Arity {
                    function: name.to_string(),
                    want: 2,
                    got: args.len(),
                });
            }
            let v = eval(&args[0], resolver)?;
            if args.len() == 2 {
                let Some(width) = eval(&args[1], resolver)?.num() else {
                    return Ok(Value::Unknown);
                };
                let Some(n) = v.num() else {
                    return Ok(Value::Unknown);
                };
                Ok(Value::Str(format!("{:>width$}", fmt_num(n), width = width as usize)))
            } else {
                match v.text() {
                    Some(s) => Ok(Value::Str(s)),
                    None => Ok(Value::Unknown),
                }
            }
        }
        "val" | "int" => {
            arity(name, args, 1)?;
            let v = eval(&args[0], resolver)?;
            match v.num() {
                Some(n) => Ok(Value::Num(if lname == "int" { n.trunc() } else { n })),
                None => Ok(Value::Num(0.0)),
            }
        }
        "round" => {
            if args.len() != 2 {
                return Err(EvalError::Arity {
                    function: name.to_string(),
                    want: 2,
                    got: args.len(),
                });
            }
            let (Some(n), Some(d)) = (
                eval(&args[0], resolver)?.num(),
                eval(&args[1], resolver)?.num(),
            ) else {
                return Ok(Value::Unknown);
            };
            let m = 10f64.powi(d as i32);
            Ok(Value::Num((n * m).round() / m))
        }
        "abs" => {
            arity(name, args, 1)?;
            match eval(&args[0], resolver)?.num() {
                Some(n) => Ok(Value::Num(n.abs())),
                None => Ok(Value::Unknown),
            }
        }
        "min" | "max" => {
            if args.len() < 2 {
                return Err(EvalError::Arity {
                    function: name.to_string(),
                    want: 2,
                    got: args.len(),
                });
            }
            let mut acc: Option<f64> = None;
            for a in args {
                let Some(n) = eval(a, resolver)?.num() else {
                    return Ok(Value::Unknown);
                };
                acc = Some(match acc {
                    None => n,
                    Some(m) => {
                        if lname == "min" {
                            m.min(n)
                        } else {
                            m.max(n)
                        }
                    }
                });
            }
            Ok(acc.map(Value::Num).unwrap_or(Value::Unknown))
        }
        "mod" => {
            if args.len() != 2 {
                return Err(EvalError::Arity {
                    function: name.to_string(),
                    want: 2,
                    got: args.len(),
                });
            }
            let (Some(a), Some(b)) = (
                eval(&args[0], resolver)?.num(),
                eval(&args[1], resolver)?.num(),
            ) else {
                return Ok(Value::Unknown);
            };
            if b == 0.0 {
                return Err(EvalError::DivideByZero);
            }
            Ok(Value::Num(a % b))
        }
        "dtos" => {
            arity(name, args, 1)?;
            match eval(&args[0], resolver)? {
                // `DTOS` renders a date as `YYYYMMDD`; the corpus only uses it
                // as an index key, so the exact rendering is pinned by the
                // Julian conversion the same expressions use.
                Value::Date(d) => Ok(Value::Str(julian_to_ymd(d))),
                Value::Unknown => Ok(Value::Unknown),
                other => match other.text() {
                    Some(s) => Ok(Value::Str(s)),
                    None => Ok(Value::Unknown),
                },
            }
        }
        _ => {
            // Everything else is a data reference. Ask the resolver first — a
            // caller that has the database can compute it — and fall back to
            // `Unknown` only when the resolver has nothing.
            let mut evaluated = Vec::with_capacity(args.len());
            for a in args {
                evaluated.push(eval(a, resolver)?);
            }
            if let Some(v) = resolver.call(name, &evaluated) {
                return Ok(v);
            }
            if DATA_FUNCTIONS.iter().any(|f| f.eq_ignore_ascii_case(name)) {
                Ok(Value::Unknown)
            } else {
                Err(EvalError::UnknownFunction {
                    name: name.to_string(),
                })
            }
        }
    }
}

/// Julian day number → `YYYYMMDD`. CaseView's `DTOS`/`mkdate`/`sdate` family is
/// Julian-based (`PERTOJ`, `JTOPER` are the corpus's own period helpers), so the
/// conversion is the standard proleptic-Gregorian one with the JDN epoch at
/// noon on 1 January 4713 BC.
pub fn julian_to_ymd(jdn: i64) -> String {
    let a = jdn + 32044;
    let b = (4 * a + 3) / 146097;
    let c = a - (146097 * b) / 4;
    let d = (4 * c + 3) / 1461;
    let e = c - (1461 * d) / 4;
    let m = (5 * e + 2) / 153;
    let day = e - (153 * m + 2) / 5 + 1;
    let month = m + 3 - 12 * (m / 10);
    let year = 100 * b + d - 4800 + m / 10;
    format!("{year:04}{month:02}{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexes_the_corpus_forms() {
        let toks = lex(r#"act("yr0:bu",ac_NO)"#).unwrap();
        assert_eq!(toks[0], Tok::Ident("act".into()));
        assert_eq!(toks[1], Tok::LParen);
        assert_eq!(toks[2], Tok::Str("yr0:bu".into()));
        assert_eq!(toks[3], Tok::Comma);
        assert_eq!(toks[4], Tok::Ident("ac_NO".into()));
        assert_eq!(toks[5], Tok::RParen);
        assert_eq!(toks[6], Tok::Eof);
    }

    #[test]
    fn single_quote_form_is_a_string() {
        let toks = lex("act('..','','.........','%s')").unwrap();
        assert_eq!(toks[2], Tok::StrS("..".into()));
        assert_eq!(toks[4], Tok::StrS("".into()));
    }

    #[test]
    fn arrow_is_distinct_from_minus() {
        let toks = lex("AM->SCHEDULE=R(0)").unwrap();
        assert_eq!(toks[0], Tok::Ident("AM".into()));
        assert_eq!(toks[1], Tok::Arrow);
        assert_eq!(toks[2], Tok::Ident("SCHEDULE".into()));
    }

    #[test]
    fn numbers_keep_fractional_and_stop_at_alias_dot() {
        assert_eq!(lex("1.0").unwrap()[0], Tok::Num("1.0".into()));
        let t = lex("EXP.B6").unwrap();
        assert_eq!(t[0], Tok::Ident("EXP".into()));
        assert_eq!(t[1], Tok::Dot);
    }

    #[test]
    fn parses_precedence() {
        let ast = parse("1+2*3").unwrap();
        assert_eq!(
            ast,
            Ast::Bin {
                op: BinOp::Add,
                left: Box::new(Ast::Num("1".into())),
                right: Box::new(Ast::Bin {
                    op: BinOp::Mul,
                    left: Box::new(Ast::Num("2".into())),
                    right: Box::new(Ast::Num("3".into())),
                }),
            }
        );
    }

    #[test]
    fn period_alias_table_is_longest_prefix_first() {
        assert_eq!(canonical_period_key("pyx3"), "yr1:fx3");
        assert_eq!(canonical_period_key("py3"), "yr13");
        assert_eq!(canonical_period_key("ajc12"), "ac12");
        assert_eq!(canonical_period_key("aj12"), "an12");
        // `cp` is digit-gated: `cpb:yr0` is not a `cp` key.
        assert_eq!(canonical_period_key("cpb:yr0"), "cpb:yr0");
        assert_eq!(canonical_period_key("cp12"), "per12");
        // Unknown prefixes pass through unchanged.
        assert_eq!(canonical_period_key("ZZ9"), "ZZ9");
    }

    #[test]
    fn corpus_expressions_parse() {
        for src in [
            r#"act("yr0:bu",ac_NO)"#,
            r#"map("cpb:yr0",MAP_NO)"#,
            r#"mkdate(clp13)"#,
            "rc(1#)=\"\"&rc(1#)=\"\"",
            r#"field("SH","AD3",docnum())"#,
            r#"GRPDESC(ID,MAP_NO,2)"#,
            r#"iif(rowcount(celltable()) <= 3,0,1)"#,
            r#"Action("InsertRow",cellTable(),"1",cellrow()+1,"Yes",1)"#,
            r#"case(rc(-13#)="1",AM->SCHEDULE=R(0),RC(-13#)="2",LTRIM(AM->GROUP2)=R(0))"#,
        ] {
            parse(src).unwrap_or_else(|e| panic!("{src}: {e}"));
        }
    }
}
