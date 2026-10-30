//! `opencw serve` — the local web shell over the same kernels the CLI fronts
//! (P5.I I3).
//!
//! The transport is `std::net::TcpListener` with one thread per connection; the
//! JSON is hand-emitted in the `tb_json` style; the frontend is three embedded
//! files (`include_str!`, no CDN, no external asset). There is no new crate
//! dependency anywhere in this module.
//!
//! Two laws hold across every route:
//!
//! 1. **Reads** go through the same `cw_cli` / `cw_app` / `cw_core` surfaces the
//!    shell renders (`TbView`, `DocumentBook`, `Chart`, `Adjustments`,
//!    `SignoffBook`, the manifest walker). Nothing is re-implemented here.
//! 2. **Writes** are always preview-then-commit. A preview runs the *real*
//!    operation against a throwaway copy of the engagement and reports the full
//!    before/after manifest diff; the engagement itself is untouched. A commit
//!    snapshots the engagement into `<parent>/work/`, runs the same operation
//!    through `cw_core::transact` (write-aside -> fsync -> atomic rename) on the
//!    live directory, and reports the diff of the snapshot against the result.
//!
//! Manifest roots per operation: in-place table edits (`entries`, `renumber`,
//! `compress`) watch the engagement directory; operations that create a new
//! directory (`yearend`, `lifecycle expand|backup`) watch the work area.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use cw_app::accounts::Chart;
use cw_app::adjustments::{AdjustmentEntry, AdjustmentLine, Adjustments, EntryType};
use cw_app::backup::{package_engagement, restore_package, PackageOptions};

use cw_app::groups::MpTable;
use cw_app::recalc::{recalc, RecalcOptions};
use cw_app::renumber::RenumberPlan;
use cw_app::signoff::{SignoffBook, SignoffStage};
use cw_app::yearend::{close_year, CloseStamp, YearEndOptions, RE_REQUIRED};
use cw_core::cube::Cube;
use cw_core::json::{self, Json};
use cw_core::sha256::sha256_hex;
use cw_core::transact;

use crate::{
    account_json, docs_json, parse_entry_type, renumber_dialog, renumber_scope, tb_json_window,
    DocsMode, Engagement, TbRequest,
};

/// Default listening port for `opencw serve`.
pub const DEFAULT_PORT: u16 = 8078;
/// Default bind address (loopback only: this is a local desktop surface).
pub const DEFAULT_BIND: &str = "127.0.0.1";

/// The shell, embedded so `serve` needs no working-directory assets.
const INDEX_HTML: &str = include_str!("../static/index.html");
const APP_JS: &str = include_str!("../static/app.js");
const STYLE_CSS: &str = include_str!("../static/style.css");

const CT_HTML: &str = "text/html; charset=utf-8";
const CT_JS: &str = "application/javascript; charset=utf-8";
const CT_CSS: &str = "text/css; charset=utf-8";
const CT_JSON: &str = "application/json; charset=utf-8";

// ---------------------------------------------------------------------------
// Server handle
// ---------------------------------------------------------------------------

/// One served engagement plus its work area (`<parent>/work/`).
#[derive(Clone, Debug)]
pub struct State {
    pub eng: Engagement,
    /// Scratch/backup root: `engagement.parent/work/`, created on demand.
    pub work: PathBuf,
    /// Canonical parent scope: every caller-supplied path must live inside it.
    root: PathBuf,
}

impl State {
    /// Bind the state to an engagement directory.
    pub fn open(dir: impl AsRef<Path>) -> Result<State, String> {
        let eng = Engagement::open(dir).map_err(|e| e.to_string())?;
        let parent = eng
            .dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| eng.dir.clone());
        let root = std::fs::canonicalize(&parent).unwrap_or_else(|_| parent.clone());
        Ok(State {
            work: parent.join("work"),
            eng,
            root,
        })
    }

    /// The engagement directory, canonical where possible.
    fn eng_dir(&self) -> PathBuf {
        std::fs::canonicalize(&self.eng.dir).unwrap_or_else(|_| self.eng.dir.clone())
    }

    /// Resolve a caller-supplied directory, refusing anything outside the
    /// engagement's parent tree (this is a local tool, but a path parameter is
    /// still an untrusted input).
    fn guarded_dir(&self, raw: &str) -> Result<PathBuf, ApiError> {
        let p = PathBuf::from(raw.trim());
        if raw.trim().is_empty() {
            return Err(ApiError::bad_request("a directory path is required"));
        }
        let canon = std::fs::canonicalize(&p)
            .map_err(|e| ApiError::not_found(format!("{}: {e}", p.display())))?;
        if !canon.is_dir() {
            return Err(ApiError::bad_request(format!(
                "{} is not a directory",
                p.display()
            )));
        }
        if !canon.starts_with(&self.root) {
            return Err(ApiError::forbidden(format!(
                "{} is outside the engagement tree ({})",
                canon.display(),
                self.root.display()
            )));
        }
        Ok(canon)
    }

    /// Create the work area on demand and return it.
    fn ensure_work(&self) -> Result<PathBuf, ApiError> {
        std::fs::create_dir_all(&self.work)
            .map_err(|e| ApiError::internal(format!("cannot create {}: {e}", self.work.display())))?;
        Ok(self.work.clone())
    }
}

/// Configuration for one `serve` run.
#[derive(Clone, Debug)]
pub struct Config {
    pub dir: PathBuf,
    pub port: u16,
    pub bind: String,
}

impl Config {
    pub fn new(dir: impl Into<PathBuf>) -> Config {
        Config {
            dir: dir.into(),
            port: DEFAULT_PORT,
            bind: DEFAULT_BIND.to_string(),
        }
    }
}

/// Serve until the process is killed. Prints the bound URL (and a curl smoke
/// line) on start.
pub fn serve(cfg: Config) -> Result<(), String> {
    let state = Arc::new(State::open(&cfg.dir)?);
    let addr = format!("{}:{}", cfg.bind, cfg.port);
    let listener =
        TcpListener::bind(&addr).map_err(|e| format!("cannot bind {addr}: {e}"))?;
    let bound = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| addr.clone());
    println!("{}", state.eng.header());
    println!("OpenCW web shell | http://{bound}/");
    println!(
        "  curl -s http://{bound}/api/overview | head -c 400\n  (Ctrl-C stops the server; work area: {})",
        state.work.display()
    );
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let st = Arc::clone(&state);
                std::thread::spawn(move || {
                    let _ = handle_connection(&st, stream);
                });
            }
            Err(e) => eprintln!("opencw serve: accept failed: {e}"),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// One parsed request.
#[derive(Clone, Debug)]
pub struct Req {
    pub method: String,
    /// Decoded path without the query string (`/api/tb`).
    pub path: String,
    /// Decoded query pairs, in order.
    pub query: Vec<(String, String)>,
    pub body: String,
}

impl Req {
    /// Build a GET request, splitting any `?query` off `target`
    /// (the socket path is already split; this keeps tests honest).
    pub fn get(target: &str) -> Req {
        let (path, query) = split_target(target);
        Req {
            method: "GET".to_string(),
            path,
            query,
            body: String::new(),
        }
    }

    pub fn post(path: &str, body: &str) -> Req {
        Req {
            method: "POST".to_string(),
            path: path.to_string(),
            query: Vec::new(),
            body: body.to_string(),
        }
    }

    /// First query value for `name`.
    fn q(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn q_usize(&self, name: &str) -> Result<Option<usize>, ApiError> {
        match self.q(name) {
            None | Some("") => Ok(None),
            Some(raw) => raw
                .trim()
                .parse::<usize>()
                .map(Some)
                .map_err(|_| ApiError::bad_request(format!("{name} must be a non-negative integer"))),
        }
    }

    fn q_i32(&self, name: &str) -> Result<Option<i32>, ApiError> {
        match self.q(name) {
            None | Some("") => Ok(None),
            Some(raw) => raw
                .trim()
                .parse::<i32>()
                .map(Some)
                .map_err(|_| ApiError::bad_request(format!("{name} must be an integer"))),
        }
    }
}

/// One response.
#[derive(Clone, Debug)]
pub struct Resp {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
    /// Raw bytes for a binary body (the `/api/docman/file` route). When present
    /// the socket writer sends these instead of `body`'s UTF-8 (which is a
    /// lossy mirror kept only so handlers stay stringly testable).
    pub raw: Option<Vec<u8>>,
    /// Optional `Content-Disposition` value (download/open-in-tab naming).
    pub disposition: Option<String>,
}

impl Resp {
    fn json(status: u16, body: String) -> Resp {
        Resp {
            status,
            content_type: CT_JSON,
            body,
            raw: None,
            disposition: None,
        }
    }

    fn text(status: u16, content_type: &'static str, body: &str) -> Resp {
        Resp {
            status,
            content_type,
            body: body.to_string(),
            raw: None,
            disposition: None,
        }
    }

    /// The bytes actually written to the socket.
    fn bytes(&self) -> &[u8] {
        match &self.raw {
            Some(b) => b,
            None => self.body.as_bytes(),
        }
    }
}

/// Structured API failure: the wire shape is `{"error":{"code","message"}}`.
#[derive(Clone, Debug)]
pub struct ApiError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
    /// Optional extra payload merged into the error body (e.g. cascade issues).
    pub extra: String,
}

impl ApiError {
    pub fn new(status: u16, code: &'static str, message: impl Into<String>) -> ApiError {
        ApiError {
            status,
            code,
            message: message.into(),
            extra: String::new(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> ApiError {
        ApiError::new(400, "bad_request", message)
    }

    pub fn not_found(message: impl Into<String>) -> ApiError {
        ApiError::new(404, "not_found", message)
    }

    pub fn forbidden(message: impl Into<String>) -> ApiError {
        ApiError::new(403, "forbidden", message)
    }

    pub fn method_not_allowed(message: impl Into<String>) -> ApiError {
        ApiError::new(405, "method_not_allowed", message)
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> ApiError {
        ApiError::new(409, code, message)
    }

    pub fn unprocessable(code: &'static str, message: impl Into<String>) -> ApiError {
        ApiError::new(422, code, message)
    }

    pub fn internal(message: impl Into<String>) -> ApiError {
        ApiError::new(500, "internal", message)
    }

    fn with_extra(mut self, extra: String) -> ApiError {
        self.extra = extra;
        self
    }

    fn render(&self) -> String {
        format!(
            "{{\"error\":{{\"code\":{},\"message\":{}}}{}}}",
            crate::json_str(self.code),
            crate::json_str(&self.message),
            if self.extra.is_empty() {
                String::new()
            } else {
                format!(",{}", self.extra)
            }
        )
    }
}

impl From<cw_core::CoreError> for ApiError {
    fn from(e: cw_core::CoreError) -> ApiError {
        match e {
            cw_core::CoreError::Io(ref io) if io.kind() == std::io::ErrorKind::NotFound => {
                ApiError::not_found(e.to_string())
            }
            cw_core::CoreError::Io(_) => ApiError::internal(e.to_string()),
            cw_core::CoreError::Format(_) => ApiError::unprocessable("invalid_data", e.to_string()),
        }
    }
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        _ => "Error",
    }
}

/// Read one request off `stream`, dispatch it, and answer it.
fn handle_connection(state: &State, mut stream: TcpStream) -> std::io::Result<()> {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(30)));
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    // Headers
    let head_end = loop {
        if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
            break pos;
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 64 * 1024 {
            return write_resp(
                &mut stream,
                &ApiError::new(413, "payload_too_large", "request headers exceed 64 KiB").render_resp(),
            );
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_ascii_uppercase();
    let target = parts.next().unwrap_or("/");

    let mut content_length = 0usize;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse::<usize>().unwrap_or(0);
            }
        }
    }
    if content_length > 8 * 1024 * 1024 {
        return write_resp(
            &mut stream,
            &ApiError::new(413, "payload_too_large", "request body exceeds 8 MiB").render_resp(),
        );
    }
    let mut body: Vec<u8> = buf[head_end + 4..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);

    let (path, query) = split_target(target);
    let req = Req {
        method,
        path,
        query,
        body: String::from_utf8_lossy(&body).to_string(),
    };
    let resp = handle(state, &req);
    write_resp(&mut stream, &resp)
}

impl ApiError {
    fn render_resp(&self) -> Resp {
        Resp::json(self.status, self.render())
    }
}

fn write_resp(stream: &mut TcpStream, resp: &Resp) -> std::io::Result<()> {
    let body = resp.bytes();
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        resp.status,
        status_text(resp.status),
        resp.content_type,
        body.len()
    );
    if let Some(d) = &resp.disposition {
        head.push_str(&format!("Content-Disposition: {d}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn find_subsequence(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// `/api/tb?a=1` -> (`/api/tb`, [(a,1)]), both percent-decoded.
fn split_target(target: &str) -> (String, Vec<(String, String)>) {
    match target.split_once('?') {
        Some((p, q)) => (pct_decode(p), parse_query(q)),
        None => (pct_decode(target), Vec::new()),
    }
}

fn parse_query(raw: &str) -> Vec<(String, String)> {
    raw.split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (pct_decode(k), pct_decode(v)),
            None => (pct_decode(pair), String::new()),
        })
        .collect()
}

fn pct_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = &raw[i + 1..i + 3];
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Dispatch one request. Pure over `state`/`req` so handlers are unit-testable
/// without a socket.
pub fn handle(state: &State, req: &Req) -> Resp {
    match route(state, req) {
        Ok(resp) => resp,
        Err(e) => Resp::json(e.status, e.render()),
    }
}

fn route(state: &State, req: &Req) -> Result<Resp, ApiError> {
    // `/` -> [], `/api/tb` -> ["api", "tb"] (repeated slashes collapse).
    let segs: Vec<&str> = req.path.split('/').filter(|s| !s.is_empty()).collect();
    let method = req.method.as_str();

    match (method, segs.as_slice()) {
        ("GET", []) => Ok(Resp::text(200, CT_HTML, INDEX_HTML)),
        ("GET", ["static", name]) => static_asset(name),
        ("GET", ["api"]) | ("GET", ["api", "health"]) => Ok(Resp::json(
            200,
            format!(
                "{{\"ok\":true,\"version\":{}}}",
                crate::json_str(env!("CARGO_PKG_VERSION"))
            ),
        )),
        ("GET", ["api", "overview"]) => overview(state, req),
        ("GET", ["api", "tb"]) => tb(state, req),
        ("GET", ["api", "docs"]) => docs(state, req),
        ("GET", ["api", "docman"]) => docman(state, req),
        ("GET", ["api", "docman", "file"]) => docman_file(state, req),
        ("GET", ["api", "docman", "preview-cvw"]) => docman_preview_cvw(state, req),
        ("GET", ["api", "account", ac_no]) => account(state, ac_no),
        ("GET", ["api", "entries"]) => entries_list(state, req),
        ("POST", ["api", "entries", phase]) => entries_op(state, req, phase),
        ("POST", ["api", "renumber", phase]) => renumber_op(state, req, phase),
        ("POST", ["api", "yearend", phase]) => yearend_op(state, req, phase),
        ("POST", ["api", "lifecycle", op]) => lifecycle_op(state, req, op, None),
        ("POST", ["api", "lifecycle", op, phase]) => lifecycle_op(state, req, op, Some(phase)),
        ("GET", ["api", "manifest"]) => manifest_route(state, req),
        ("POST", ["api", "manifest", "diff"]) => manifest_diff_route(state, req),
        ("GET", ["api", "signoff"]) => signoff_route(state, req),
        (m, _) if m != "GET" && m != "POST" => Err(ApiError::method_not_allowed(format!(
            "{m} is not supported (GET/POST only)"
        ))),
        (_, segs) => Err(ApiError::not_found(format!(
            "no route for {} /{}",
            method,
            segs.join("/")
        ))),
    }
}

fn static_asset(name: &str) -> Result<Resp, ApiError> {
    match name {
        "index.html" | "" => Ok(Resp::text(200, CT_HTML, INDEX_HTML)),
        "app.js" => Ok(Resp::text(200, CT_JS, APP_JS)),
        "style.css" => Ok(Resp::text(200, CT_CSS, STYLE_CSS)),
        other => Err(ApiError::not_found(format!(
            "/static/{other} is not an embedded asset (index.html, app.js, style.css)"
        ))),
    }
}

// ---------------------------------------------------------------------------
// GET /api/overview
// ---------------------------------------------------------------------------

fn overview(state: &State, _req: &Req) -> Result<Resp, ApiError> {
    let eng = &state.eng;
    // Two severities. A core table that will not decode is an inconsistency
    // (`healthy:false`). The `MP` map space is an optional surface the renumber
    // cascade itself tolerates (a truncated/missing `MP` is documented corpus
    // state, F63), so it is reported as a warning and leaves health alone.
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    let (am_rows, am_posting) = match Chart::from_table(&eng.table("AM")?) {
        Ok(chart) => (chart.len().to_string(), chart.posting().count().to_string()),
        Err(e) => {
            errors.push(format!("\"am\":{}", crate::json_str(&e.to_string())));
            ("null".to_string(), "null".to_string())
        }
    };
    let mp_schemes = match eng.table("MP").and_then(|t| MpTable::from_table(&t)) {
        Ok(mp) => mp.schemes().count().to_string(),
        Err(e) => {
            warnings.push(format!("\"mp\":{}", crate::json_str(&e.to_string())));
            "null".to_string()
        }
    };
    let (sh_live, sh_deleted, sh_forms) = match crate::docman::open_book(&eng.dir, &eng.stem) {
        Ok(book) => {
            let c = book.census();
            (c.live.to_string(), c.deleted.to_string(), c.forms.to_string())
        }
        Err(e) => {
            errors.push(format!("\"sh\":{}", crate::json_str(&e.to_string())));
            ("null".to_string(), "null".to_string(), "null".to_string())
        }
    };
    let bl_rows = match eng.table("BL").and_then(|t| Cube::from_table(&t)) {
        Ok(cube) => cube.len().to_string(),
        Err(e) => {
            errors.push(format!("\"bl\":{}", crate::json_str(&e.to_string())));
            "null".to_string()
        }
    };
    let gl_rows = match eng.table("GL") {
        Ok(t) => t.records.len().to_string(),
        Err(e) => {
            errors.push(format!("\"gl\":{}", crate::json_str(&e.to_string())));
            "null".to_string()
        }
    };
    let healthy = errors.is_empty();
    let body = format!(
        "{{\"engagement\":{},\"stem\":{},\"path\":{},\"version\":{},\"census\":{{\"am_posting\":{},\"am_rows\":{},\"mp_schemes\":{},\"sh_live\":{},\"sh_deleted\":{},\"sh_forms\":{},\"bl_rows\":{},\"gl_rows\":{}}},\"healthy\":{},\"errors\":{{{}}},\"warnings\":{{{}}}}}",
        crate::json_str(&eng.dir.display().to_string()),
        crate::json_str(&eng.stem),
        crate::json_str(&eng.dir.display().to_string()),
        crate::json_str(env!("CARGO_PKG_VERSION")),
        am_posting,
        am_rows,
        mp_schemes,
        sh_live,
        sh_deleted,
        sh_forms,
        bl_rows,
        gl_rows,
        healthy,
        errors.join(","),
        warnings.join(",")
    );
    Ok(Resp::json(200, body))
}

// ---------------------------------------------------------------------------
// GET /api/tb
// ---------------------------------------------------------------------------

fn tb(state: &State, req: &Req) -> Result<Resp, ApiError> {
    let mut tbr = TbRequest::default();
    if let Some(y) = req.q_i32("year")? {
        tbr.year = y;
    }
    if let Some(p) = req.q_usize("period")? {
        tbr.period = p as u8;
    }
    if let Some(g) = req.q("group") {
        if !g.is_empty() {
            tbr.group = g.to_string();
        }
    }
    if let Some(e) = req.q("entity") {
        if !e.is_empty() {
            tbr.entity = Some(e.to_string());
        }
    }
    tbr.synthetics = req.q("synthetics").map(|v| v == "1" || v == "true").unwrap_or(false);
    let limit = req.q_usize("limit")?.unwrap_or(0);
    let offset = req.q_usize("offset")?.unwrap_or(0);
    let body = tb_json_window(&state.eng, &tbr, offset, limit)?;
    Ok(Resp::json(200, body))
}

// ---------------------------------------------------------------------------
// GET /api/docs
// ---------------------------------------------------------------------------

fn docs(state: &State, req: &Req) -> Result<Resp, ApiError> {
    let mode = DocsMode::parse(req.q("mode").unwrap_or("tree"))?;
    let limit = req.q_usize("limit")?.unwrap_or(0);
    Ok(Resp::json(200, docs_json(&state.eng, mode, limit)?))
}

// ---------------------------------------------------------------------------
// GET /api/docman, /api/docman/file, /api/docman/preview-cvw
// ---------------------------------------------------------------------------
//
// The Document Manager is the *file-centric* lens: every physical file in the
// engagement directory (walked with the package path's skip rules) LEFT-JOINed
// to the `SH` register by filename stem / CVID. `docman.rs` owns the model and
// the documented join law; the routes only parse query options and pick a
// representation.

fn docman(state: &State, req: &Req) -> Result<Resp, ApiError> {
    let raw = req.q("path").unwrap_or("");
    let dir = if raw.trim().is_empty() {
        state.eng_dir()
    } else {
        state.guarded_dir(raw)?
    };
    let options = crate::docman::Options {
        internals: req
            .q("internals")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false),
        class: req.q("class").map(str::to_string),
        q: req.q("q").map(str::to_string),
        sha: req
            .q("sha")
            .map(|v| !(v == "0" || v == "false"))
            .unwrap_or(true),
    };
    let d = crate::docman::collect(&dir, &state.eng.stem, options)?;
    Ok(Resp::json(200, crate::docman::json(&d)))
}

/// Resolve a caller-supplied path inside the engagement directory: no absolute
/// paths, no `..`, and the canonical result must still be under the engagement.
fn resolve_in_engagement(state: &State, raw: &str) -> Result<(PathBuf, String), ApiError> {
    let rel = raw.trim();
    if rel.is_empty() {
        return Err(ApiError::bad_request("a file path is required"));
    }
    if Path::new(rel).is_absolute() {
        return Err(ApiError::bad_request(
            "path must be relative to the engagement directory",
        ));
    }
    let base = state.eng_dir();
    let joined = base.join(rel);
    let canon = std::fs::canonicalize(&joined)
        .map_err(|e| ApiError::not_found(format!("{}: {e}", rel)))?;
    if !canon.starts_with(&base) {
        return Err(ApiError::forbidden(format!(
            "{rel} resolves outside the engagement directory ({})",
            base.display()
        )));
    }
    if !canon.is_file() {
        return Err(ApiError::bad_request(format!("{rel} is not a file")));
    }
    let shown = canon
        .strip_prefix(&base)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| rel.to_string());
    Ok((canon, shown))
}

fn docman_file(state: &State, req: &Req) -> Result<Resp, ApiError> {
    let (path, _) = resolve_in_engagement(state, req.q("path").unwrap_or(""))?;
    let bytes = std::fs::read(&path)
        .map_err(|e| ApiError::internal(format!("cannot read {}: {e}", path.display())))?;
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let mime = crate::docman::mime_for(&ext);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "document".to_string());
    Ok(Resp {
        status: 200,
        content_type: mime,
        body: String::from_utf8_lossy(&bytes).into_owned(),
        disposition: Some(format!(
            "inline; filename*=UTF-8''{}",
            crate::docman::pct_encode_path(&name)
        )),
        raw: Some(bytes),
    })
}

fn docman_preview_cvw(state: &State, req: &Req) -> Result<Resp, ApiError> {
    let (path, shown) = resolve_in_engagement(state, req.q("path").unwrap_or(""))?;
    let is_cv = path
        .extension()
        .map(|e| {
            let e = e.to_string_lossy().to_ascii_lowercase();
            e == "cvw" || e == "cvv"
        })
        .unwrap_or(false);
    if !is_cv {
        return Err(ApiError::bad_request(format!(
            "{shown} is not a CaseView document (.cvw/.cvv)"
        )));
    }
    let p = crate::docman::preview(&path)?;
    if req.q("format") == Some("json") {
        return Ok(Resp::json(200, crate::docman::preview_json(&p, &shown)));
    }
    let raw_url = format!(
        "/api/docman/file?path={}",
        crate::docman::pct_encode_path(&shown)
    );
    Ok(Resp::text(
        200,
        CT_HTML,
        &crate::docman::preview_html(&p, &shown, &raw_url),
    ))
}

// ---------------------------------------------------------------------------
// GET /api/account/<ac_no>
// ---------------------------------------------------------------------------

fn account(state: &State, ac_no: &str) -> Result<Resp, ApiError> {
    let chart = Chart::from_table(&state.eng.table("AM")?)?;
    if crate::find_account(&chart, ac_no).is_none() {
        return Err(ApiError::not_found(format!(
            "account {ac_no:?} is not in the AM chart"
        )));
    }
    Ok(Resp::json(200, account_json(&state.eng, ac_no)?))
}

// ---------------------------------------------------------------------------
// GET /api/entries
// ---------------------------------------------------------------------------

fn entries_list(state: &State, req: &Req) -> Result<Resp, ApiError> {
    let acct = req.q("acct").unwrap_or("").trim().to_string();
    let limit = req.q_usize("limit")?.unwrap_or(0);
    let offset = req.q_usize("offset")?.unwrap_or(0);
    let gl = state.eng.table("GL")?;
    let journal = Adjustments::from_table(&gl)?;
    let chart = Chart::from_table(&state.eng.table("AM")?).ok();
    let desc_of = |acct: &str| -> String {
        chart
            .as_ref()
            .and_then(|c| crate::find_account(c, acct))
            .map(|a| a.desc.clone())
            .unwrap_or_default()
    };

    let matching: Vec<&AdjustmentEntry> = journal
        .entries()
        .iter()
        .filter(|e| {
            acct.is_empty()
                || e.system_id.trim() == acct
                || e.lines
                    .iter()
                    .any(|l| l.account.trim() == acct.as_str())
        })
        .collect();

    let mut out = String::from("{");
    out.push_str(&format!(
        "\"engagement\":{},\"acct\":{},\"count\":{},\"offset\":{},\"limit\":{},\"entries\":[",
        crate::json_str(&state.eng.stem),
        crate::json_str(&acct),
        matching.len(),
        offset,
        limit
    ));
    let start = offset.min(matching.len());
    let end = match limit {
        0 => matching.len(),
        n => (start + n).min(matching.len()),
    };
    for (i, e) in matching[start..end].iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let balance = e.balance();
        let total = if balance.debits != 0.0 {
            balance.debits
        } else {
            balance.credits
        };
        out.push_str(&format!(
            "{{\"ref\":{},\"system_id\":{},\"date\":{},\"period\":{},\"desc\":{},\"type_code\":{},\"type_label\":{},\"journal\":{},\"source\":{},\"status\":{},\"balanced\":{},\"total\":{:.2},\"difference\":{:.2},\"line_count\":{},\"lines\":[",
            crate::json_str(&e.system_id),
            crate::json_str(&e.system_id),
            crate::json_str(&e.date),
            match e.period {
                Some(p) => p.to_string(),
                None => "null".to_string(),
            },
            crate::json_str(&e.description),
            e.entry_type.code(),
            crate::json_str(e.entry_type.display_name()),
            crate::json_str(&e.journal_source),
            crate::json_str(&e.journal_source),
            match e.entry_type.status() {
                Some(s) => crate::json_str(s),
                None => "null".to_string(),
            },
            e.is_balanced(),
            total,
            e.difference(),
            e.lines.len()
        ));
        for (j, l) in e.lines.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"row\":{},\"acct\":{},\"desc\":{},\"amount\":{:.2},\"dc\":{},\"period\":{},\"type_code\":{},\"type_label\":{}}}",
                l.row,
                crate::json_str(l.account.trim()),
                crate::json_str(&desc_of(&l.account)),
                l.amount,
                crate::json_str(if l.amount >= 0.0 { "DR" } else { "CR" }),
                match l.period {
                    Some(p) => p.to_string(),
                    None => "null".to_string(),
                },
                e.entry_type.code(),
                crate::json_str(e.entry_type.display_name())
            ));
        }
        out.push_str("]}");
    }
    out.push_str("]}");
    Ok(Resp::json(200, out))
}

// ---------------------------------------------------------------------------
// Manifest service
// ---------------------------------------------------------------------------

/// One file in a manifest: path-relative name, byte length, SHA-256.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
}

/// One file that exists on both sides with different bytes/digests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    pub name: String,
    pub a_bytes: u64,
    pub b_bytes: u64,
    pub a_sha: String,
    pub b_sha: String,
}

/// Result of comparing two manifests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ManifestDiff {
    pub only_a: Vec<FileEntry>,
    pub only_b: Vec<FileEntry>,
    pub changed: Vec<ChangedFile>,
}

impl ManifestDiff {
    pub fn is_empty(&self) -> bool {
        self.only_a.is_empty() && self.only_b.is_empty() && self.changed.is_empty()
    }

    fn render(&self) -> String {
        let mut out = String::from("{\"only_a\":[");
        push_files(&mut out, &self.only_a);
        out.push_str("],\"only_b\":[");
        push_files(&mut out, &self.only_b);
        out.push_str("],\"changed\":[");
        for (i, c) in self.changed.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"name\":{},\"a_bytes\":{},\"b_bytes\":{},\"a_sha\":{},\"b_sha\":{}}}",
                crate::json_str(&c.name),
                c.a_bytes,
                c.b_bytes,
                crate::json_str(&c.a_sha),
                crate::json_str(&c.b_sha)
            ));
        }
        out.push_str("]}");
        out
    }
}

fn push_files(out: &mut String, files: &[FileEntry]) {
    for (i, f) in files.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"name\":{},\"bytes\":{},\"sha256\":{}}}",
            crate::json_str(&f.name),
            f.bytes,
            crate::json_str(&f.sha256)
        ));
    }
}

/// Recursive manifest of one directory, names relative to it with `/`
/// separators, sorted by name.
pub fn manifest_of(dir: &Path) -> Result<Vec<FileEntry>, ApiError> {
    let mut files: Vec<FileEntry> = Vec::new();
    walk(dir, "", &mut files, 0)?;
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(files)
}

const MAX_DEPTH: usize = 32;

fn walk(
    root: &Path,
    prefix: &str,
    out: &mut Vec<FileEntry>,
    depth: usize,
) -> Result<(), ApiError> {
    if depth > MAX_DEPTH {
        return Err(ApiError::internal(format!(
            "manifest: directory nesting exceeds {MAX_DEPTH} at {prefix}"
        )));
    }
    let entries = std::fs::read_dir(root)
        .map_err(|e| ApiError::internal(format!("cannot list {}: {e}", root.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| ApiError::internal(format!("cannot list {}: {e}", root.display())))?;
        let name = entry.file_name().to_string_lossy().to_string();
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let path = entry.path();
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            walk(&path, &rel, out, depth + 1)?;
        } else if meta.is_file() {
            let bytes = std::fs::read(&path).map_err(|e| {
                ApiError::internal(format!("cannot read {}: {e}", path.display()))
            })?;
            out.push(FileEntry {
                name: rel,
                bytes: bytes.len() as u64,
                sha256: sha256_hex(&bytes),
            });
        }
    }
    Ok(())
}

/// Compare two manifests by name.
pub fn diff_manifests(a: &[FileEntry], b: &[FileEntry]) -> ManifestDiff {
    let am: BTreeMap<&str, &FileEntry> = a.iter().map(|f| (f.name.as_str(), f)).collect();
    let bm: BTreeMap<&str, &FileEntry> = b.iter().map(|f| (f.name.as_str(), f)).collect();
    let mut diff = ManifestDiff::default();
    for (name, fa) in &am {
        match bm.get(name) {
            None => diff.only_a.push((*fa).clone()),
            Some(fb) => {
                if fa.sha256 != fb.sha256 || fa.bytes != fb.bytes {
                    diff.changed.push(ChangedFile {
                        name: (*name).to_string(),
                        a_bytes: fa.bytes,
                        b_bytes: fb.bytes,
                        a_sha: fa.sha256.clone(),
                        b_sha: fb.sha256.clone(),
                    });
                }
            }
        }
    }
    for (name, fb) in &bm {
        if !am.contains_key(name) {
            diff.only_b.push((*fb).clone());
        }
    }
    diff
}

/// Manifest of one or more roots, names prefixed by a label when there is more
/// than one root (so `engagement/BL.dbf` cannot collide with `work/BL.dbf`).
fn manifest_roots(roots: &[(String, PathBuf)]) -> Result<Vec<FileEntry>, ApiError> {
    if roots.len() == 1 {
        return manifest_of(&roots[0].1);
    }
    let mut out: Vec<FileEntry> = Vec::new();
    for (label, path) in roots {
        if !path.exists() {
            continue;
        }
        for f in manifest_of(path)? {
            out.push(FileEntry {
                name: format!("{label}/{}", f.name),
                bytes: f.bytes,
                sha256: f.sha256,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn manifest_route(state: &State, req: &Req) -> Result<Resp, ApiError> {
    let raw = req.q("path").unwrap_or("");
    let dir = if raw.trim().is_empty() {
        state.eng_dir()
    } else {
        state.guarded_dir(raw)?
    };
    let files = manifest_of(&dir)?;
    let total: u64 = files.iter().map(|f| f.bytes).sum();
    let mut out = format!(
        "{{\"path\":{},\"total_bytes\":{},\"count\":{},\"files\":[",
        crate::json_str(&dir.display().to_string()),
        total,
        files.len()
    );
    push_files(&mut out, &files);
    out.push_str("]}");
    Ok(Resp::json(200, out))
}

fn manifest_diff_route(state: &State, req: &Req) -> Result<Resp, ApiError> {
    let body = parse_json_body(req)?;
    let a = str_field(&body, "a")?
        .ok_or_else(|| ApiError::bad_request("body needs \"a\": <directory>"))?;
    let b = str_field(&body, "b")?
        .ok_or_else(|| ApiError::bad_request("body needs \"b\": <directory>"))?;
    let da = state.guarded_dir(&a)?;
    let db = state.guarded_dir(&b)?;
    let diff = diff_manifests(&manifest_of(&da)?, &manifest_of(&db)?);
    let out = format!(
        "{{\"a\":{},\"b\":{},\"only_a\":{},\"only_b\":{},\"changed\":{}}}",
        crate::json_str(&da.display().to_string()),
        crate::json_str(&db.display().to_string()),
        diff_slice_json(&diff.only_a),
        diff_slice_json(&diff.only_b),
        diff_changed_json(&diff.changed)
    );
    Ok(Resp::json(200, out))
}

fn diff_slice_json(files: &[FileEntry]) -> String {
    let mut out = String::from("[");
    push_files(&mut out, files);
    out.push(']');
    out
}

fn diff_changed_json(changed: &[ChangedFile]) -> String {
    let mut out = String::from("[");
    for (i, c) in changed.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"name\":{},\"a_bytes\":{},\"b_bytes\":{},\"a_sha\":{},\"b_sha\":{}}}",
            crate::json_str(&c.name),
            c.a_bytes,
            c.b_bytes,
            crate::json_str(&c.a_sha),
            crate::json_str(&c.b_sha)
        ));
    }
    out.push(']');
    out
}

// ---------------------------------------------------------------------------
// GET /api/signoff
// ---------------------------------------------------------------------------

fn signoff_route(state: &State, req: &Req) -> Result<Resp, ApiError> {
    let key = req.q("doc").unwrap_or("").trim().to_string();
    if key.is_empty() {
        return Err(ApiError::bad_request("signoff needs ?doc=<guid|schedule>"));
    }
    let path = state.eng.table_path("SH")?;
    let book = SignoffBook::open(&path)?;
    let sheet = book
        .sheet(&key)
        .or_else(|| {
            book.sheets()
                .iter()
                .find(|s| s.schedule == key)
        })
        .ok_or_else(|| ApiError::not_found(format!("no SH document for {key:?} (guid or schedule)")))?;
    let stage = match sheet.stage() {
        SignoffStage::Unsigned => "unsigned",
        SignoffStage::Prepared => "prepared",
        SignoffStage::Reviewed => "reviewed",
    };
    let mut out = format!(
        "{{\"doc\":{{\"guid\":{},\"schedule\":{},\"title\":{},\"prg\":{},\"name\":{},\"type\":{},\"sign\":{},\"lockdown\":{}}},\"stage\":{},\"complete\":{},\"highest_signed\":{},\"next_level\":{},\"signed_count\":{},\"levels\":[",
        crate::json_str(&sheet.guid),
        crate::json_str(&sheet.schedule),
        crate::json_str(&sheet.title),
        crate::json_str(&sheet.prg),
        crate::json_str(&sheet.name),
        crate::json_str(&sheet.doc_type),
        sheet.sign,
        sheet.locked_down,
        crate::json_str(stage),
        sheet.is_complete(),
        match sheet.highest_signed() {
            Some(l) => l.to_string(),
            None => "null".to_string(),
        },
        match sheet.next_level() {
            Some(l) => l.to_string(),
            None => "null".to_string(),
        },
        sheet.signed_count()
    );
    for (i, lvl) in sheet.levels.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"level\":{},\"role\":{},\"signed\":{},\"initials\":{},\"user\":{},\"date\":{},\"signature_guid\":{}}}",
            lvl.level,
            crate::json_str(lvl.role_name()),
            lvl.is_signed(),
            crate::json_str(lvl.initials_text()),
            crate::json_str(lvl.user()),
            crate::json_str(&lvl.date),
            crate::json_str(lvl.signature_guid())
        ));
    }
    out.push_str("]}");
    Ok(Resp::json(200, out))
}

// ---------------------------------------------------------------------------
// Operations: shared preview/commit machinery
// ---------------------------------------------------------------------------

/// Process-wide scratch counter (timestamp collisions under parallel requests).
static SCRATCH: AtomicU64 = AtomicU64::new(0);

fn scratch_tag(op: &str) -> String {
    let n = SCRATCH.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{op}-{}-{nanos}-{n}", std::process::id())
}

/// Recursive copy of a directory tree.
fn copy_tree(src: &Path, dst: &Path) -> Result<(), ApiError> {
    std::fs::create_dir_all(dst)
        .map_err(|e| ApiError::internal(format!("cannot create {}: {e}", dst.display())))?;
    let entries = std::fs::read_dir(src)
        .map_err(|e| ApiError::internal(format!("cannot list {}: {e}", src.display())))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| ApiError::internal(format!("cannot list {}: {e}", src.display())))?;
        let target = dst.join(entry.file_name());
        let meta = entry
            .metadata()
            .map_err(|e| ApiError::internal(format!("cannot stat {}: {e}", entry.path().display())))?;
        if meta.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if meta.is_file() {
            std::fs::copy(entry.path(), &target).map_err(|e| {
                ApiError::internal(format!("cannot copy {}: {e}", entry.path().display()))
            })?;
        }
    }
    Ok(())
}

fn remove_tree(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

/// Run `f` against a throwaway copy of the engagement, then discard the copy.
///
/// This is what makes a preview trustworthy: the operation runs through the
/// very same code path as a commit (including its `transact` writes), only on a
/// copy, so the engagement bytes are provably untouched.
fn in_copy<T>(
    state: &State,
    tag: &str,
    f: impl FnOnce(&Engagement) -> Result<T, ApiError>,
) -> Result<T, ApiError> {
    let work = state.ensure_work()?;
    let scratch = work.join(scratch_tag(tag));
    copy_tree(&state.eng_dir(), &scratch)?;
    let result = (|| {
        let eng = Engagement::open(&scratch).map_err(ApiError::from)?;
        f(&eng)
    })();
    remove_tree(&scratch);
    result
}

/// Snapshot the watched roots, run a committing operation, and diff.
fn commit_with_diff<T>(
    state: &State,
    roots: &[(String, PathBuf)],
    f: impl FnOnce() -> Result<T, ApiError>,
) -> Result<(T, ManifestDiff, Option<PathBuf>), ApiError> {
    let before = manifest_roots(roots)?;
    let backup = if roots.iter().any(|(_, p)| *p == state.eng_dir()) {
        let work = state.ensure_work()?;
        let dir = work.join(format!("{}-backup-{}", sanitize(&state.eng.stem), scratch_tag("bak")));
        copy_tree(&state.eng_dir(), &dir)?;
        Some(dir)
    } else {
        None
    };
    let value = f()?;
    let after = manifest_roots(roots)?;
    let diff = diff_manifests(&before, &after);
    Ok((value, diff, backup))
}

/// Filesystem-safe token from a stem (Windows forbids `:` `?` `*` ...).
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '-',
            c if (c as u32) < 0x20 => '-',
            c => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn op_envelope(op: &str, phase: &str, payload_key: &str, payload: &str, diff: &ManifestDiff) -> String {
    let _ = phase;
    format!(
        "{{\"ok\":true,\"op\":{},\"{}\":{},\"manifest\":{}}}",
        crate::json_str(op),
        payload_key,
        payload,
        diff.render()
    )
}

// ---------------------------------------------------------------------------
// POST /api/entries/{preview,commit}
// ---------------------------------------------------------------------------

struct EntrySpec {
    system_id: String,
    description: String,
    user_reference: String,
    entry_type: EntryType,
    date: String,
    period: Option<u8>,
    lines: Vec<(String, f64)>,
}

fn parse_entry_spec(req: &Req) -> Result<EntrySpec, ApiError> {
    let body = parse_json_body(req)?;
    let system_id = str_field(&body, "ref")?
        .or(str_field(&body, "system_id")?)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::bad_request("body needs \"ref\" (entry reference)"))?;
    let date = str_field(&body, "date")?
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::bad_request("body needs \"date\" as YYYYMMDD"))?;
    if date.len() != 8 || !date.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ApiError::bad_request(format!(
            "date {date:?} is not YYYYMMDD"
        )));
    }
    let description = str_field(&body, "desc")?.unwrap_or_default();
    let user_reference = str_field(&body, "from_ref")?
        .or(str_field(&body, "user_reference")?)
        .unwrap_or_default();
    let type_raw = str_field(&body, "type")?.unwrap_or_else(|| "N".to_string());
    let entry_type = parse_entry_type(&type_raw)?;
    let period = match body.get("period") {
        None | Some(Json::Null) => None,
        Some(Json::Num(n)) => Some(*n as u8),
        Some(Json::Str(s)) if s.trim().is_empty() => None,
        Some(Json::Str(s)) => Some(s.trim().parse::<u8>().map_err(|_| {
            ApiError::bad_request(format!("period {s:?} is not 1..=12"))
        })?),
        Some(_) => return Err(ApiError::bad_request("period must be a number 1..=12")),
    };
    let lines_json = body
        .get("lines")
        .and_then(Json::as_arr)
        .ok_or_else(|| ApiError::bad_request("body needs \"lines\": [{acct, amount}, ...]"))?;
    let mut lines: Vec<(String, f64)> = Vec::new();
    for (i, line) in lines_json.iter().enumerate() {
        let acct = line
            .get("acct")
            .and_then(Json::as_str)
            .or_else(|| line.get("account").and_then(Json::as_str))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ApiError::bad_request(format!("line {i}: \"acct\" is required")))?;
        let amount = match line.get("amount") {
            Some(Json::Num(n)) => *n,
            Some(Json::Str(s)) => s
                .trim()
                .trim_start_matches('$')
                .parse::<f64>()
                .map_err(|_| ApiError::bad_request(format!("line {i}: amount {s:?} is not numeric")))?,
            _ => return Err(ApiError::bad_request(format!("line {i}: \"amount\" is required"))),
        };
        if !amount.is_finite() {
            return Err(ApiError::bad_request(format!("line {i}: amount is not finite")));
        }
        lines.push((acct, amount));
    }
    if lines.is_empty() {
        return Err(ApiError::bad_request("an entry needs at least one line"));
    }
    Ok(EntrySpec {
        system_id,
        description,
        user_reference,
        entry_type,
        date,
        period,
        lines,
    })
}

/// Apply one entry: GL through the shell writer, then the F35 `BL` recalc.
///
/// `AM` is never touched (F35: the `AM` balance columns are a dead cache).
fn apply_entry(eng: &Engagement, spec: &EntrySpec, write: bool) -> Result<String, ApiError> {
    let chart = Chart::from_table(&eng.table("AM")?)?;
    for (acct, _) in &spec.lines {
        if crate::find_account(&chart, acct).is_none() {
            return Err(ApiError::unprocessable(
                "unknown_account",
                format!("line account {acct:?} is not in the AM chart"),
            ));
        }
    }
    // Validation happens inside `AdjustmentEntry::render` (balance + row law).
    let entry = AdjustmentEntry::render(
        &spec.system_id,
        &spec.user_reference,
        &spec.description,
        spec.entry_type,
        &spec.date,
        spec.period,
        &spec
            .lines
            .iter()
            .map(|(acct, amount)| AdjustmentLine::new(acct.trim(), *amount))
            .collect::<Vec<_>>(),
    )?;
    let balance = entry.balance();

    let gl_before = eng.table("GL")?.records.len();
    let bl_before = eng.table("BL")?.records.len();

    crate::edit_entry(
        eng,
        &spec.system_id,
        &spec.description,
        &spec.user_reference,
        spec.entry_type,
        &spec.date,
        spec.period,
        &spec.lines,
        write,
    )?;

    // F35: recompute the balance cube from the journal (idempotent).
    let mut bl = eng.table("BL")?;
    let journal = Adjustments::from_table(&eng.table("GL")?)?;
    let recalc_report = recalc(&mut bl, &journal, &RecalcOptions::default())?;
    let bl_after = bl.records.len();
    if write {
        transact::commit_if_changed(&eng.table_path("BL")?, &bl.encode())?;
    }
    let gl_after = eng.table("GL")?.records.len();

    let mut out = String::from("{");
    out.push_str(&format!(
        "\"entry\":{{\"ref\":{},\"system_id\":{},\"date\":{},\"desc\":{},\"type\":{},\"type_code\":{},\"type_label\":{},\"period\":{},\"from_ref\":{}}},",
        crate::json_str(&entry.system_id),
        crate::json_str(&entry.system_id),
        crate::json_str(&entry.date),
        crate::json_str(&entry.description),
        crate::json_str(entry.entry_type.label()),
        entry.entry_type.code(),
        crate::json_str(entry.entry_type.display_name()),
        match entry.period {
            Some(p) => p.to_string(),
            None => "null".to_string(),
        },
        crate::json_str(&entry.user_reference)
    ));
    out.push_str(&format!(
        "\"balanced\":{},\"difference\":{:.2},\"debits\":{:.2},\"credits\":{:.2},\"line_count\":{},\"lines\":[",
        balance.is_balanced(),
        balance.net(),
        balance.debits,
        balance.credits,
        entry.lines.len()
    ));
    for (i, line) in entry.lines.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let desc = crate::find_account(&chart, &line.account)
            .map(|a| a.desc.clone())
            .unwrap_or_default();
        out.push_str(&format!(
            "{{\"acct\":{},\"desc\":{},\"amount\":{:.2},\"dc\":{}}}",
            crate::json_str(line.account.trim()),
            crate::json_str(&desc),
            line.amount,
            crate::json_str(if line.amount >= 0.0 { "DR" } else { "CR" })
        ));
    }
    out.push_str(&format!(
        "],\"rows\":[{{\"table\":\"GL\",\"before\":{gl_before},\"after\":{gl_after}}},{{\"table\":\"BL\",\"before\":{bl_before},\"after\":{bl_after}}}],\
\"bl\":{{\"y_rows_written\":{},\"y_rows_updated\":{},\"y_rows_tombstoned\":{},\"ytd_stamped\":{},\"o_rows_touched\":{}}}}}",
        recalc_report.y_rows_written,
        recalc_report.y_rows_updated,
        recalc_report.y_rows_tombstoned,
        recalc_report.ytd_stamped,
        recalc_report.o_rows_touched
    ));
    Ok(out)
}

fn entries_op(state: &State, req: &Req, phase: &str) -> Result<Resp, ApiError> {
    let spec = parse_entry_spec(req)?;
    let eng_dir = state.eng_dir();
    match phase {
        "preview" => {
            let (payload, diff) = in_copy(state, "entries-preview", |eng| {
                let payload = apply_entry(eng, &spec, true)?;
                let after = manifest_of(&eng.dir)?;
                let before = manifest_of(&eng_dir)?;
                Ok((payload, diff_manifests(&before, &after)))
            })?;
            Ok(Resp::json(
                200,
                op_envelope("entries", "preview", "preview", &payload, &diff),
            ))
        }
        "commit" => {
            let (payload, diff, backup) = commit_with_diff(
                state,
                &[("engagement".to_string(), eng_dir)],
                || apply_entry(&state.eng, &spec, true),
            )?;
            let mut body = op_envelope("entries", "commit", "result", &payload, &diff);
            body = insert_backup(body, backup.as_deref());
            Ok(Resp::json(200, body))
        }
        other => Err(ApiError::not_found(format!(
            "/api/entries/{other}: expected preview|commit"
        ))),
    }
}

fn insert_backup(mut body: String, backup: Option<&Path>) -> String {
    if let Some(p) = backup {
        // `body` ends with the closing `}` of the envelope.
        body.pop();
        body.push_str(&format!(
            ",\"backup\":{}}}",
            crate::json_str(&p.display().to_string())
        ));
    }
    body
}

// ---------------------------------------------------------------------------
// POST /api/renumber/{preview,commit}
// ---------------------------------------------------------------------------

fn parse_renumber_plan(state: &State, req: &Req) -> Result<RenumberPlan, ApiError> {
    let body = parse_json_body(req)?;
    if let Some(plan) = body.get("plan").and_then(Json::as_arr) {
        let mut pairs: Vec<(String, String)> = Vec::new();
        for (i, item) in plan.iter().enumerate() {
            let from = item
                .get("from")
                .and_then(Json::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| ApiError::bad_request(format!("plan[{i}]: \"from\" is required")))?;
            let to = item
                .get("to")
                .and_then(Json::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| ApiError::bad_request(format!("plan[{i}]: \"to\" is required")))?;
            pairs.push((from, to));
        }
        if pairs.is_empty() {
            return Err(ApiError::bad_request("plan is empty"));
        }
        return Ok(RenumberPlan::from_pairs(&pairs)?);
    }
    let delta = match body.get("delta") {
        Some(Json::Num(n)) => *n as i64,
        Some(Json::Str(s)) => s
            .trim()
            .parse::<i64>()
            .map_err(|_| ApiError::bad_request(format!("delta {s:?} is not an integer")))?,
        _ => {
            return Err(ApiError::bad_request(
                "body needs \"plan\": [{from, to}] or \"delta\": N",
            ))
        }
    };
    let scope = renumber_scope(&state.eng)?;
    Ok(RenumberPlan::from_delta(&scope, delta)?)
}

fn renumber_payload(outcome: &crate::RenumberOutcome) -> String {
    let mut out = String::from("{\"blocked\":");
    out.push_str(if outcome.blocked() { "true" } else { "false" });
    out.push_str(",\"changed\":");
    out.push_str(if outcome.changed() { "true" } else { "false" });
    out.push_str(",\"written\":[");
    for (i, t) in outcome.written.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&crate::json_str(&format!("{t}.dbf")));
    }
    out.push_str("],\"renames\":[");
    for (i, r) in outcome.plan.renames().iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"from\":{},\"to\":{}}}",
            crate::json_str(&r.from),
            crate::json_str(&r.to)
        ));
    }
    out.push_str(&format!(
        "],\"cells\":{},\"skipped\":[",
        outcome.report.cells()
    ));
    for (i, s) in outcome.plan.skipped().iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&crate::json_str(s));
    }
    out.push_str("],\"tables\":[");
    for (i, (table, cells, rows)) in outcome.report.table_stats().iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"table\":{},\"cells\":{},\"rows\":{},\"before\":{},\"after\":{}}}",
            crate::json_str(table),
            cells,
            rows,
            cells,
            cells
        ));
    }
    out.push_str("],\"changed_tables\":[");
    for (i, (table, changed)) in outcome.changed_tables.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"table\":{},\"changed\":{}}}",
            crate::json_str(&format!("{table}.dbf")),
            changed
        ));
    }
    out.push_str("],\"dangling_before\":");
    out.push_str(&outcome.report.dangling_before.len().to_string());
    out.push_str(",\"dangling_after\":");
    out.push_str(&outcome.report.dangling_after.len().to_string());
    out.push_str(",\"already_applied\":[");
    for (i, s) in outcome.report.already_applied.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&crate::json_str(s));
    }
    out.push_str("],\"issues\":[");
    for (i, issue) in outcome.issues.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&crate::json_str(&issue.to_string()));
    }
    out.push_str("]}");
    out
}

fn renumber_op(state: &State, req: &Req, phase: &str) -> Result<Resp, ApiError> {
    let plan = parse_renumber_plan(state, req)?;
    let eng_dir = state.eng_dir();
    match phase {
        "preview" => {
            let (payload, diff, outcome) = in_copy(state, "renumber-preview", |eng| {
                let outcome = renumber_dialog(eng, &plan, 0, true)?;
                let payload = renumber_payload(&outcome);
                let after = manifest_of(&eng.dir)?;
                let before = manifest_of(&eng_dir)?;
                Ok((payload, diff_manifests(&before, &after), outcome))
            })?;
            if outcome.blocked() {
                return Err(ApiError::conflict(
                    "cascade_refused",
                    first_issue_message(&outcome),
                )
                .with_extra(format!("\"preview\":{payload}")));
            }
            Ok(Resp::json(
                200,
                op_envelope("renumber", "preview", "preview", &payload, &diff),
            ))
        }
        "commit" => {
            let (outcome, diff, backup) = commit_with_diff(
                state,
                &[("engagement".to_string(), eng_dir)],
                || Ok(renumber_dialog(&state.eng, &plan, 0, true)?),
            )?;
            let payload = renumber_payload(&outcome);
            if outcome.blocked() {
                return Err(ApiError::conflict(
                    "cascade_refused",
                    first_issue_message(&outcome),
                )
                .with_extra(format!("\"preview\":{payload}")));
            }
            let body = op_envelope("renumber", "commit", "result", &payload, &diff);
            Ok(Resp::json(200, insert_backup(body, backup.as_deref())))
        }
        other => Err(ApiError::not_found(format!(
            "/api/renumber/{other}: expected preview|commit"
        ))),
    }
}

fn first_issue_message(outcome: &crate::RenumberOutcome) -> String {
    match outcome.issues.split_first() {
        None => "the renumber cascade was refused".to_string(),
        Some((first, rest)) => format!(
            "the renumber cascade was refused ({} blocking issue(s)); first: {first}",
            rest.len() + 1
        ),
    }
}

// ---------------------------------------------------------------------------
// POST /api/yearend/{preview,commit}
// ---------------------------------------------------------------------------

struct YearendRequest {
    opts: YearEndOptions,
    dest_root: PathBuf,
    dest_stem: String,
    destination_display: String,
}

fn parse_yearend(state: &State, req: &Req) -> Result<YearendRequest, ApiError> {
    let body = parse_json_body(req)?;
    let re = str_field(&body, "retained_earning")?
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let re = match re {
        Some(a) => a,
        None => discover_retained_earning(state)?.ok_or_else(|| {
            ApiError::unprocessable("retained_earning_required", RE_REQUIRED)
        })?,
    };
    let dest_raw = str_field(&body, "dest")?.unwrap_or_default();
    let dest_raw = dest_raw.trim().to_string();
    let (dest_root, dest_stem) = if dest_raw.is_empty() {
        let work = state.ensure_work()?;
        (
            work,
            format!("{}-yearend-{}", sanitize(&state.eng.stem), short_stamp()),
        )
    } else {
        let p = PathBuf::from(&dest_raw);
        let stem = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ApiError::bad_request(format!("dest {dest_raw:?} has no directory name")))?;
        let parent = p
            .parent()
            .filter(|p2| !p2.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let parent = std::fs::canonicalize(&parent).map_err(|e| {
            ApiError::bad_request(format!("dest parent {}: {e}", parent.display()))
        })?;
        if !parent.starts_with(&state.root) {
            return Err(ApiError::forbidden(format!(
                "dest {} is outside the engagement tree ({})",
                parent.display(),
                state.root.display()
            )));
        }
        (parent, stem)
    };
    let destination_display = dest_root.join(&dest_stem).display().to_string();
    Ok(YearendRequest {
        opts: YearEndOptions::balance_carry(re),
        dest_root,
        dest_stem,
        destination_display,
    })
}

/// Deterministic-ish destination token (`YYYYMMDDhhmmss` local-free).
fn short_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

/// Find an account whose description names retained earnings (only when the
/// caller did not name one; a wrong guess is impossible because the close
/// validates the account against the chart).
fn discover_retained_earning(state: &State) -> Result<Option<String>, ApiError> {
    let chart = match Chart::from_table(&state.eng.table("AM")?) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    let hit = chart.posting().find(|a| {
        let d = a.desc.to_ascii_lowercase();
        d.contains("retained earn") || d.contains("retained")
    });
    Ok(hit.map(|a| a.ac_no.trim().to_string()))
}

fn yearend_payload(report: &cw_app::yearend::CloseReport, dest: &str) -> String {
    let mut out = format!(
        "{{\"destination\":{},\"source_stem\":{},\"dest_stem\":{},\"retained_earning\":{},\"rows\":[",
        crate::json_str(dest),
        crate::json_str(&report.source_stem),
        crate::json_str(&report.dest_stem),
        crate::json_str(&report.retained_earning_account)
    );
    for (i, (table, d)) in report.tables.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"table\":\"{table}.dbf\",\"before\":{},\"after\":{},\"src_live\":{},\"dst_live\":{},\"src_deleted\":{},\"dst_deleted\":{}}}",
            d.src_live, d.dst_live, d.src_live, d.dst_live, d.src_deleted, d.dst_deleted
        ));
    }
    out.push_str(&format!(
        "],\"key_totals\":{{\"net_income\":{:.2},\"net_income_cents\":{},\"closing_accounts\":{},\"p_twins\":{}}},\"tables\":{}",
        report.net_income_cents as f64 / 100.0,
        report.net_income_cents,
        report.closing_accounts,
        report.p_twins,
        report.tables.len()
    ));
    out.push_str(",\"files\":[");
    for (i, f) in report.files.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&crate::json_str(f));
    }
    out.push_str("],\"notes\":[");
    for (i, n) in report.notes.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&crate::json_str(n));
    }
    out.push_str("]}");
    out
}

fn yearend_op(state: &State, req: &Req, phase: &str) -> Result<Resp, ApiError> {
    let request = parse_yearend(state, req)?;
    let stamp = CloseStamp::default();
    let work_roots = || vec![("work".to_string(), state.work.clone())];
    match phase {
        "preview" => {
            // Build the close into a scratch root and report the files it would
            // create, with digests. Nothing is written to the work area.
            let preview_root = state
                .ensure_work()?
                .join(scratch_tag("yearend-preview"));
            std::fs::create_dir_all(&preview_root)
                .map_err(|e| ApiError::internal(format!("cannot create {}: {e}", preview_root.display())))?;
            let report = close_year(
                &state.eng.dir,
                &preview_root,
                &request.dest_stem,
                &request.opts,
                &stamp,
            )?;
            let produced = manifest_of(&preview_root.join(&request.dest_stem));
            remove_tree(&preview_root);
            let produced = produced?;
            let diff = diff_manifests(&[], &produced);
            let payload = yearend_payload(&report, &request.destination_display);
            Ok(Resp::json(
                200,
                op_envelope("yearend", "preview", "preview", &payload, &diff),
            ))
        }
        "commit" => {
            let before = manifest_roots(&work_roots())?;
            let report = close_year(
                &state.eng.dir,
                &request.dest_root,
                &request.dest_stem,
                &request.opts,
                &stamp,
            )?;
            let after = manifest_roots(&work_roots())?;
            let diff = diff_manifests(&before, &after);
            let payload = yearend_payload(&report, &request.destination_display);
            let mut body = op_envelope("yearend", "commit", "result", &payload, &diff);
            body = insert_backup(
                body,
                Some(&request.dest_root.join(&request.dest_stem)),
            );
            Ok(Resp::json(200, body))
        }
        other => Err(ApiError::not_found(format!(
            "/api/yearend/{other}: expected preview|commit"
        ))),
    }
}

// ---------------------------------------------------------------------------
// POST /api/lifecycle/{expand|compress|backup}[/{preview|commit}]
// ---------------------------------------------------------------------------

fn lifecycle_op(
    state: &State,
    req: &Req,
    op: &str,
    phase: Option<&str>,
) -> Result<Resp, ApiError> {
    let body = parse_json_body(req)?;
    // Both client shapes are accepted: the phase in the path, or `mode` in the
    // body (`{"mode":"preview"|"commit"}`).
    let phase = match phase {
        Some(p) => p.to_string(),
        None => str_field(&body, "mode")?
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "preview".to_string()),
    };
    if phase != "preview" && phase != "commit" {
        return Err(ApiError::bad_request(format!(
            "mode {phase:?} is not preview|commit"
        )));
    }
    let path_raw = str_field(&body, "path")?.unwrap_or_default();
    let dest_raw = str_field(&body, "dest")?.unwrap_or_default();
    match op {
        "expand" => lifecycle_expand(state, &path_raw, &dest_raw, &phase),
        "compress" => lifecycle_compress(state, &path_raw, &dest_raw, &phase),
        "backup" => lifecycle_backup(state, &path_raw, &dest_raw, &phase),
        other => Err(ApiError::not_found(format!(
            "/api/lifecycle/{other}: expected expand|compress|backup"
        ))),
    }
}

fn tables_payload(pairs: &[(String, usize, usize)]) -> String {
    let mut out = String::from("[");
    for (i, (table, before, after)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"table\":{},\"before\":{},\"after\":{}}}",
            crate::json_str(table),
            before,
            after
        ));
    }
    out.push(']');
    out
}

/// Locate the `.ac_` package: an explicit file, or the single package in a dir.
fn find_package(raw: &str, state: &State) -> Result<PathBuf, ApiError> {
    if raw.trim().is_empty() {
        // Default: the engagement's own working directory, when it has been
        // compressed in place.
        let dir = state.eng_dir();
        return single_package(&dir);
    }
    let p = PathBuf::from(raw.trim());
    let p = if p.is_relative() {
        state.root.join(p)
    } else {
        p
    };
    let canon = std::fs::canonicalize(&p)
        .map_err(|e| ApiError::not_found(format!("{}: {e}", p.display())))?;
    if !canon.starts_with(&state.root) {
        return Err(ApiError::forbidden(format!(
            "{} is outside the engagement tree ({})",
            canon.display(),
            state.root.display()
        )));
    }
    if canon.is_dir() {
        single_package(&canon)
    } else if canon
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase().ends_with(".ac_"))
        .unwrap_or(false)
    {
        Ok(canon)
    } else {
        Err(ApiError::bad_request(format!(
            "{} is not a .ac_ package",
            canon.display()
        )))
    }
}

fn single_package(dir: &Path) -> Result<PathBuf, ApiError> {
    let mut hits: Vec<PathBuf> = Vec::new();
    let entries = std::fs::read_dir(dir)
        .map_err(|e| ApiError::not_found(format!("cannot list {}: {e}", dir.display())))?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if name.ends_with(".ac_") {
            hits.push(entry.path());
        }
    }
    hits.sort();
    match hits.len() {
        0 => Err(ApiError::not_found(format!(
            "no .ac_ package in {}",
            dir.display()
        ))),
        1 => Ok(hits.remove(0)),
        _ => Err(ApiError::bad_request(format!(
            "{} holds {} packages; name the one to expand",
            dir.display(),
            hits.len()
        ))),
    }
}

fn lifecycle_expand(
    state: &State,
    path_raw: &str,
    dest_raw: &str,
    phase: &str,
) -> Result<Resp, ApiError> {
    let pkg = find_package(path_raw, state)?;
    let pkg_stem = pkg
        .file_stem()
        .map(|s| sanitize(&s.to_string_lossy()))
        .unwrap_or_else(|| "package".to_string());
    let dest = if dest_raw.trim().is_empty() {
        state.ensure_work()?.join(format!("{pkg_stem}-expanded-{}", short_stamp()))
    } else {
        let p = PathBuf::from(dest_raw.trim());
        let p = if p.is_relative() { state.root.join(p) } else { p };
        let parent = p
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| state.root.clone());
        let parent = std::fs::canonicalize(&parent)
            .unwrap_or_else(|_| state.ensure_work().unwrap_or_else(|_| state.root.clone()));
        if !parent.starts_with(&state.root) {
            return Err(ApiError::forbidden(format!(
                "dest {} is outside the engagement tree ({})",
                parent.display(),
                state.root.display()
            )));
        }
        parent.join(
            p.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| pkg_stem.clone()),
        )
    };
    let payload = |dest: &Path, members: &[(String, usize, usize)], stem: &str, crc: usize| {
        format!(
            "{{\"op\":\"expand\",\"source\":{},\"destination\":{},\"stem\":{},\"tables\":{},\"members\":{},\"crc_mismatch\":{}}}",
            crate::json_str(&pkg.display().to_string()),
            crate::json_str(&dest.display().to_string()),
            crate::json_str(stem),
            tables_payload(members),
            members.len(),
            crc
        )
    };
    match phase {
        "preview" => {
            let scratch = state.ensure_work()?.join(scratch_tag("expand-preview"));
            let report = restore_package(&pkg, &scratch)?;
            let produced = manifest_of(&scratch)?;
            remove_tree(&scratch);
            let members = member_pairs(&report.members);
            let body = payload(&dest, &members, &report.stem, report.crc_mismatch.len());
            let diff = diff_manifests(&[], &produced);
            Ok(Resp::json(
                200,
                op_envelope("lifecycle/expand", "preview", "preview", &body, &diff),
            ))
        }
        "commit" => {
            let before = manifest_roots(&[("work".to_string(), state.work.clone())])?;
            let report = restore_package(&pkg, &dest)?;
            let after = manifest_roots(&[("work".to_string(), state.work.clone())])?;
            let diff = diff_manifests(&before, &after);
            let members = member_pairs(&report.members);
            let body = payload(&dest, &members, &report.stem, report.crc_mismatch.len());
            let body = op_envelope("lifecycle/expand", "commit", "result", &body, &diff);
            Ok(Resp::json(200, insert_backup(body, Some(&dest))))
        }
        _ => Err(ApiError::bad_request("mode is not preview|commit")),
    }
}

fn member_pairs(members: &[cw_app::backup::PackageEntry]) -> Vec<(String, usize, usize)> {
    members
        .iter()
        .map(|m| (m.name.clone(), 0usize, m.size))
        .collect()
}

fn lifecycle_compress(
    state: &State,
    path_raw: &str,
    _dest_raw: &str,
    phase: &str,
) -> Result<Resp, ApiError> {
    let dir = if path_raw.trim().is_empty() {
        state.eng_dir()
    } else {
        state.guarded_dir(path_raw)?
    };
    let opts = PackageOptions::default();
    let payload = |pkg: &Path, stem: &str, entries: usize, bytes: u64| {
        format!(
            "{{\"op\":\"compress\",\"source\":{},\"destination\":{},\"stem\":{},\"tables\":[],\"members\":{},\"bytes\":{}}}",
            crate::json_str(&dir.display().to_string()),
            crate::json_str(&pkg.display().to_string()),
            crate::json_str(stem),
            entries,
            bytes
        )
    };
    match phase {
        "preview" => {
            let scratch = state.ensure_work()?.join(scratch_tag("compress-preview"));
            copy_tree(&dir, &scratch)?;
            let result = package_engagement(&scratch, &opts);
            let report = match result {
                Ok(r) => r,
                Err(e) => {
                    remove_tree(&scratch);
                    return Err(ApiError::from(e));
                }
            };
            let produced = manifest_of(&scratch);
            remove_tree(&scratch);
            let bytes = std::fs::metadata(&report.pkg_path)
                .map(|m| m.len())
                .unwrap_or(0);
            let _ = bytes;
            // The produced package lived in the scratch tree; its digest is in
            // `produced`.
            let produced = produced?;
            let pkg_rel = report
                .pkg_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let produced: Vec<FileEntry> = produced
                .into_iter()
                .filter(|f| f.name == pkg_rel)
                .collect();
            let size = produced.first().map(|f| f.bytes).unwrap_or(0);
            let body = payload(&dir.join(&pkg_rel), &report.stem, report.entries.len(), size);
            let diff = diff_manifests(&[], &produced);
            Ok(Resp::json(
                200,
                op_envelope("lifecycle/compress", "preview", "preview", &body, &diff),
            ))
        }
        "commit" => {
            let before = manifest_of(&dir)?;
            let report = package_engagement(&dir, &opts)?;
            let after = manifest_of(&dir)?;
            let diff = diff_manifests(&before, &after);
            let size = std::fs::metadata(&report.pkg_path)
                .map(|m| m.len())
                .unwrap_or(0);
            let body = payload(&report.pkg_path, &report.stem, report.entries.len(), size);
            let body = op_envelope("lifecycle/compress", "commit", "result", &body, &diff);
            Ok(Resp::json(200, body))
        }
        _ => Err(ApiError::bad_request("mode is not preview|commit")),
    }
}

fn lifecycle_backup(
    state: &State,
    path_raw: &str,
    dest_raw: &str,
    phase: &str,
) -> Result<Resp, ApiError> {
    let src = if path_raw.trim().is_empty() {
        state.eng_dir()
    } else {
        state.guarded_dir(path_raw)?
    };
    let dest = if dest_raw.trim().is_empty() {
        state.ensure_work()?.join(format!(
            "{}-backup-{}",
            sanitize(&state.eng.stem),
            short_stamp()
        ))
    } else {
        let p = PathBuf::from(dest_raw.trim());
        let p = if p.is_relative() { state.root.join(p) } else { p };
        let parent = p.parent().map(Path::to_path_buf).unwrap_or_else(|| state.root.clone());
        let parent = std::fs::canonicalize(&parent)
            .unwrap_or_else(|_| state.ensure_work().unwrap_or_else(|_| state.root.clone()));
        if !parent.starts_with(&state.root) {
            return Err(ApiError::forbidden(format!(
                "dest {} is outside the engagement tree ({})",
                parent.display(),
                state.root.display()
            )));
        }
        parent.join(
            p.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "backup".to_string()),
        )
    };
    let payload = |dest: &Path, files: usize| {
        format!(
            "{{\"op\":\"backup\",\"source\":{},\"destination\":{},\"tables\":[],\"files\":{}}}",
            crate::json_str(&src.display().to_string()),
            crate::json_str(&dest.display().to_string()),
            files
        )
    };
    match phase {
        "preview" => {
            let scratch = state.ensure_work()?.join(scratch_tag("backup-preview"));
            copy_tree(&src, &scratch)?;
            let produced = manifest_of(&scratch)?;
            remove_tree(&scratch);
            let body = payload(&dest, produced.len());
            let diff = diff_manifests(&[], &produced);
            Ok(Resp::json(
                200,
                op_envelope("lifecycle/backup", "preview", "preview", &body, &diff),
            ))
        }
        "commit" => {
            let before = manifest_roots(&[("work".to_string(), state.work.clone())])?;
            copy_tree(&src, &dest)?;
            let after = manifest_roots(&[("work".to_string(), state.work.clone())])?;
            let diff = diff_manifests(&before, &after);
            let files = manifest_of(&dest)?.len();
            let body = payload(&dest, files);
            let body = op_envelope("lifecycle/backup", "commit", "result", &body, &diff);
            Ok(Resp::json(200, insert_backup(body, Some(&dest))))
        }
        _ => Err(ApiError::bad_request("mode is not preview|commit")),
    }
}

// ---------------------------------------------------------------------------
// Request body helpers
// ---------------------------------------------------------------------------

fn parse_json_body(req: &Req) -> Result<Json, ApiError> {
    if req.body.trim().is_empty() {
        return Ok(Json::Obj(Vec::new()));
    }
    json::parse(&req.body).map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))
}

fn str_field(body: &Json, name: &str) -> Result<Option<String>, ApiError> {
    match body.get(name) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Str(s)) => Ok(Some(s.clone())),
        Some(_) => Err(ApiError::bad_request(format!(
            "{name} must be a string"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use cw_app::accounts::AM_SCHEMA;
    use cw_app::adjustments::GL_SCHEMA;
    use cw_app::groups::MP_SCHEMA;
    use cw_core::dbf::{Field, Table};

    // -- synthetic engagement ------------------------------------------------

    /// Build a dBase table from `(name, kind, len, dec)` descriptors.
    fn blank_table(fields: &[(String, u8, u8, u8)], rows: usize) -> Table {
        let mut header: Vec<u8> = Vec::new();
        header.push(0x03);
        header.extend_from_slice(&[0x7A, 0x0A, 0x16]);
        header.extend_from_slice(&(rows as u32).to_le_bytes());
        let header_len = 32 + 32 * fields.len() + 1;
        let record_len: usize = 1 + fields.iter().map(|f| f.2 as usize).sum::<usize>();
        header.extend_from_slice(&(header_len as u16).to_le_bytes());
        header.extend_from_slice(&(record_len as u16).to_le_bytes());
        header.extend(std::iter::repeat(0u8).take(20));
        for (name, kind, len, dec) in fields {
            let mut d = [0u8; 32];
            let bytes = name.as_bytes();
            d[..bytes.len().min(11)].copy_from_slice(&bytes[..bytes.len().min(11)]);
            d[11] = *kind;
            d[16] = *len;
            d[17] = *dec;
            header.extend_from_slice(&d);
        }
        header.push(0x0D);
        let records: Vec<Vec<u8>> = (0..rows)
            .map(|_| {
                let mut r = vec![b' '; record_len];
                r[0] = b' ';
                r
            })
            .collect();
        Table {
            raw_header: header,
            version: 0x03,
            header_len: header_len as u16,
            record_len: record_len as u16,
            fields: fields
                .iter()
                .map(|(name, kind, len, dec)| Field {
                    name: name.clone(),
                    kind: *kind,
                    len: *len,
                    dec: *dec,
                })
                .collect(),
            records,
        }
    }

    fn am_fields() -> Vec<(String, u8, u8, u8)> {
        AM_SCHEMA
            .iter()
            .map(|f| (f.name.to_string(), f.kind, f.len, f.dec))
            .collect()
    }

    fn gl_fields() -> Vec<(String, u8, u8, u8)> {
        GL_SCHEMA
            .iter()
            .map(|f| (f.name.to_string(), f.kind, f.len, f.dec))
            .collect()
    }

    fn mp_fields() -> Vec<(String, u8, u8, u8)> {
        MP_SCHEMA
            .iter()
            .map(|f| (f.name.to_string(), f.kind, f.len, f.dec))
            .collect()
    }

    fn bl_fields() -> Vec<(String, u8, u8, u8)> {
        [
            ("ID", b'C', 1u8, 0u8),
            ("GROUP", b'C', 1, 0),
            ("NUMBER", b'C', 40, 0),
            ("ENTITY", b'C', 40, 0),
            ("YEAR", b'C', 1, 0),
            ("BUCKET", b'C', 1, 0),
            ("TYPE", b'C', 1, 0),
            ("BOOKED", b'C', 1, 0),
            ("PERIOD", b'C', 2, 0),
            ("CUSTOM", b'C', 2, 0),
            ("AMT", b'N', 19, 2),
            ("AMT_YTD", b'N', 19, 2),
            ("AMT_DIST", b'N', 19, 2),
            ("FX", b'N', 19, 2),
            ("FX_YTD", b'N', 19, 2),
            ("FX_DIST", b'N', 19, 2),
            ("CWGUID", b'C', 22, 0),
            ("CWDATE", b'C', 14, 0),
            ("CREATEDON", b'C', 14, 0),
        ]
        .iter()
        .map(|(n, k, l, d)| (n.to_string(), *k, *l, *d))
        .collect()
    }

    /// Write one cell, right-aligned for numeric/date columns.
    fn put(t: &mut Table, row: usize, col: &str, value: &str) {
        let idx = t.field_index(col).expect("test column exists");
        let (start, len, kind) = {
            let mut start = 1usize;
            for f in &t.fields[..idx] {
                start += f.len as usize;
            }
            (start, t.fields[idx].len as usize, t.fields[idx].kind)
        };
        let rec = &mut t.records[row];
        for b in &mut rec[start..start + len] {
            *b = b' ';
        }
        let bytes = value.as_bytes();
        let n = bytes.len().min(len);
        if kind == b'N' || kind == b'D' {
            rec[start + len - n..start + len].copy_from_slice(&bytes[..n]);
        } else {
            rec[start..start + n].copy_from_slice(&bytes[..n]);
        }
    }

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(tag: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "ocw-web-{tag}-{}-{nanos}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn golden(tag: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("golden")
            .join("dbf")
            .join(tag)
    }

    /// The `SH` widths, taken from the frozen corpus when it is present (the
    /// schema is 124 columns wide and hand-copying it into a test would rot).
    fn sh_fields_from_corpus() -> Option<Vec<(String, u8, u8, u8)>> {
        let dir = golden("corp2026");
        let path = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().to_ascii_uppercase().ends_with("SH.DBF"))
                    .unwrap_or(false)
            })?;
        let t = Table::parse_path(&path).ok()?;
        Some(
            t.fields
                .iter()
                .map(|f| (f.name.clone(), f.kind, f.len, f.dec))
                .collect(),
        )
    }

    /// A blank engagement: two AM posting accounts, one BL row, an empty GL
    /// journal, an empty MP map space and (when the corpus is present) an empty
    /// `SH` table with authentic widths.
    fn blank_engagement(tag: &str) -> PathBuf {
        // The engagement lives in `<scratch>/eng`, so its work area
        // (`<parent>/work`, the shipped layout) is private to the test.
        let root = temp_dir(tag);
        let dir = root.join("eng");
        std::fs::create_dir_all(&dir).expect("engagement dir");
        let stem = "SYNTH";
        let mut am = blank_table(&am_fields(), 2);
        for (row, ac_no, desc, kind) in [
            (0usize, "1000", "Cash", "B"),
            (1usize, "4000", "Revenue", "I"),
        ] {
            put(&mut am, row, "LINENO", &(row + 1).to_string());
            put(&mut am, row, "TYPE", kind);
            put(&mut am, row, "AC_NO", ac_no);
            put(&mut am, row, "AC_DESC", desc);
            put(&mut am, row, "ENTITY", "E1");
            put(&mut am, row, "POST", "Y");
            put(&mut am, row, "PRINT", "Y");
            put(&mut am, row, "CWGUID", &format!("GUIDAM{row:016}"));
        }
        let mut bl = blank_table(&bl_fields(), 1);
        put(&mut bl, 0, "ID", "O");
        put(&mut bl, 0, "GROUP", "A");
        put(&mut bl, 0, "NUMBER", "1000");
        put(&mut bl, 0, "ENTITY", "E1");
        put(&mut bl, 0, "YEAR", "0");
        put(&mut bl, 0, "BUCKET", "O");
        put(&mut bl, 0, "PERIOD", "0");
        put(&mut bl, 0, "AMT", "250.00");
        put(&mut bl, 0, "AMT_YTD", "250.00");
        put(&mut bl, 0, "CWGUID", "GUIDBL00000000000001");

        let gl = blank_table(&gl_fields(), 0);
        let mp = blank_table(&mp_fields(), 0);
        let tables: Vec<(&str, &Table)> = vec![("AM", &am), ("BL", &bl), ("GL", &gl), ("MP", &mp)];
        for (sfx, t) in tables {
            std::fs::write(dir.join(format!("{stem}{sfx}.dbf")), t.encode()).expect("write table");
        }
        if let Some(fields) = sh_fields_from_corpus() {
            let sh = blank_table(&fields, 0);
            std::fs::write(dir.join(format!("{stem}SH.dbf")), sh.encode()).expect("write SH");
        }
        // The `.ac` hub: `compress`/`yearend` identify the engagement by it.
        std::fs::write(dir.join(format!("{stem}.ac")), b"AC-HUB").expect("write hub");
        dir
    }

    fn state_for(tag: &str) -> (PathBuf, State) {
        let dir = blank_engagement(tag);
        let st = State::open(&dir).expect("state opens");
        (dir, st)
    }

    // -- routing -------------------------------------------------------------

    #[test]
    fn every_contract_route_is_bound() {
        let (_dir, st) = state_for("routes");
        let ac_no = "1000";
        let doc = "nope";
        let cases: Vec<(Req, u16)> = vec![
            (Req::get("/"), 200),
            (Req::get("/static/app.js"), 200),
            (Req::get("/static/style.css"), 200),
            (Req::get("/api/overview"), 200),
            (Req::get("/api/tb?year=0&period=12&group=A&limit=0"), 200),
            (Req::get("/api/docs?mode=tree"), 200),
            (Req::get("/api/docs?mode=list"), 200),
            (Req::get("/api/docs?mode=census"), 200),
            (Req::get(&format!("/api/account/{ac_no}")), 200),
            (Req::get("/api/entries"), 200),
            (Req::get(&format!("/api/signoff?doc={doc}")), 404),
            (Req::get("/api/manifest"), 200),
            (
                Req::get("/api/does-not-exist"),
                404,
            ),
        ];
        for (req, want) in cases {
            let resp = handle(&st, &req);
            assert_eq!(
                resp.status, want,
                "{} {} -> {} ({}); body {}",
                req.method, req.path, resp.status, want, resp.body
            );
            assert!(resp.body.starts_with('{') || req.path.starts_with('/'));
        }
        // Content types are pinned per asset.
        assert_eq!(handle(&st, &Req::get("/")).content_type, CT_HTML);
        assert_eq!(handle(&st, &Req::get("/static/app.js")).content_type, CT_JS);
        assert_eq!(handle(&st, &Req::get("/static/style.css")).content_type, CT_CSS);
        assert_eq!(handle(&st, &Req::get("/api/overview")).content_type, CT_JSON);
    }

    #[test]
    fn unknown_static_and_unknown_method_are_enveloped() {
        let (_dir, st) = state_for("errors");
        let resp = handle(&st, &Req::get("/static/missing.js"));
        assert_eq!(resp.status, 404);
        assert!(resp.body.contains("\"error\":{\"code\":\"not_found\""));

        let mut req = Req::get("/api/overview");
        req.method = "DELETE".to_string();
        let resp = handle(&st, &req);
        assert_eq!(resp.status, 405);
        assert!(resp.body.contains("\"code\":\"method_not_allowed\""));
    }

    #[test]
    fn query_strings_are_percent_decoded() {
        let parsed = parse_query("acct=RSM%20-%2001&limit=5&flag");
        assert_eq!(
            parsed,
            vec![
                ("acct".to_string(), "RSM - 01".to_string()),
                ("limit".to_string(), "5".to_string()),
                ("flag".to_string(), String::new())
            ]
        );
        assert_eq!(pct_decode("a+b%2Fc"), "a b/c");
    }

    // -- read surfaces -------------------------------------------------------

    #[test]
    fn overview_reports_the_census_and_health() {
        let (_dir, st) = state_for("overview");
        let resp = handle(&st, &Req::get("/api/overview"));
        assert_eq!(resp.status, 200);
        for needle in [
            "\"census\":{",
            "\"am_posting\":2",
            "\"am_rows\":2",
            "\"bl_rows\":1",
            "\"gl_rows\":0",
            "\"stem\":\"SYNTH\"",
            "\"healthy\":true",
            "\"warnings\":{",
        ] {
            assert!(resp.body.contains(needle), "missing {needle} in {}", resp.body);
        }
    }

    /// A missing `MP` is an optional-surface warning, not an inconsistency: the
    /// frozen corpus ships a truncated `MP` (F63) and must still read healthy.
    #[test]
    fn overview_treats_a_missing_mp_as_a_warning() {
        let (dir, st) = state_for("overview-mp");
        std::fs::remove_file(dir.join("SYNTHMP.dbf")).unwrap();
        let resp = handle(&st, &Req::get("/api/overview"));
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(resp.body.contains("\"healthy\":true"), "{}", resp.body);
        assert!(resp.body.contains("\"mp_schemes\":null"), "{}", resp.body);
        assert!(resp.body.contains("\"errors\":{}"), "{}", resp.body);
        assert!(resp.body.contains("\"warnings\":{\"mp\":"), "{}", resp.body);
    }

    #[test]
    fn tb_route_pages_with_a_total() {
        let (_dir, st) = state_for("tb");
        let resp = handle(&st, &Req::get("/api/tb?year=0&period=12&group=A&limit=0"));
        assert_eq!(resp.status, 200);
        assert!(resp.body.contains("\"rows\":["), "{}", resp.body);
        assert!(resp.body.contains("\"total\":1"), "{}", resp.body);
        assert!(resp.body.contains("\"columns\":["), "{}", resp.body);

        // A page of zero rows still reports the unpaged total.
        let mut paged = Req::get("/api/tb?year=0&period=12&group=A&limit=0");
        paged.query = vec![
            ("year".into(), "0".into()),
            ("period".into(), "12".into()),
            ("group".into(), "A".into()),
            ("limit".into(), "1".into()),
            ("offset".into(), "5".into()),
        ];
        let resp = handle(&st, &paged);
        assert_eq!(resp.status, 200);
        assert!(resp.body.contains("\"total\":1"), "{}", resp.body);
        assert!(resp.body.contains("\"offset\":1"), "{}", resp.body);
        assert!(resp.body.contains("\"rows\":[]"), "{}", resp.body);

        // Bad numbers are a 400, not a panic.
        let bad = Req {
            method: "GET".into(),
            path: "/api/tb".into(),
            query: vec![("period".into(), "nope".into())],
            body: String::new(),
        };
        assert_eq!(handle(&st, &bad).status, 400);
    }

    #[test]
    fn account_route_serves_the_dialog_and_404s_unknown() {
        let (_dir, st) = state_for("account");
        let resp = handle(&st, &Req::get("/api/account/1000"));
        assert_eq!(resp.status, 200);
        assert!(resp.body.contains("\"properties\":{"), "{}", resp.body);
        assert!(resp.body.contains("\"POST\""), "{}", resp.body);
        for key in ["\"open\":", "\"ytd\":", "\"acct\":\"1000\""] {
            assert!(resp.body.contains(key), "missing {key}");
        }
        let missing = handle(&st, &Req::get("/api/account/9999"));
        assert_eq!(missing.status, 404);
        assert!(missing.body.contains("\"code\":\"not_found\""));
    }

    #[test]
    fn docs_route_serves_every_mode() {
        let (_dir, st) = state_for("docs");
        let census = handle(&st, &Req::get("/api/docs?mode=census"));
        assert_eq!(census.status, 200, "{}", census.body);
        assert!(census.body.contains("\"mode\":\"census\""), "{}", census.body);
        let tree = handle(&st, &Req::get("/api/docs?mode=tree"));
        assert_eq!(tree.status, 200, "{}", tree.body);
        assert!(tree.body.contains("\"rows\":["), "{}", tree.body);
        let list = handle(&st, &Req::get("/api/docs?mode=list"));
        assert_eq!(list.status, 200);
        assert!(list.body.contains("\"mode\":\"list\""));
        let bad = handle(&st, &Req::get("/api/docs?mode=nope"));
        assert_eq!(bad.status, 422);
    }

    #[test]
    fn entries_route_groups_headers_and_lines() {
        let (_dir, st) = state_for("entries-list");
        let resp = handle(&st, &Req::get("/api/entries"));
        assert_eq!(resp.status, 200);
        assert!(resp.body.contains("\"count\":0"), "{}", resp.body);
        assert!(resp.body.contains("\"entries\":[]"), "{}", resp.body);
    }

    // -- manifest ------------------------------------------------------------

    #[test]
    fn manifest_and_diff_agree_with_the_kernel_digest() {
        let (dir, st) = state_for("manifest");
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(dir.join("nested/x.bin"), b"payload").unwrap();
        let resp = handle(&st, &Req::get("/api/manifest"));
        assert_eq!(resp.status, 200);
        let want = sha256_hex(b"payload");
        assert!(resp.body.contains("\"nested/x.bin\""), "{}", resp.body);
        assert!(resp.body.contains(&want), "{}", resp.body);
        assert!(resp.body.contains("\"total_bytes\":"));

        // Second snapshot: one file changed, one added, one removed. It lives
        // beside the engagement (inside the guarded tree the server enforces).
        let other = dir
            .parent()
            .unwrap()
            .join("snapshot-b");
        std::fs::create_dir_all(other.join("nested")).unwrap();
        std::fs::write(other.join("nested/x.bin"), b"payload2").unwrap();
        std::fs::write(other.join("new.txt"), b"new").unwrap();
        let body = format!(
            "{{\"a\":{},\"b\":{}}}",
            crate::json_str(&dir.display().to_string()),
            crate::json_str(&other.display().to_string())
        );
        let resp = handle(&st, &Req::post("/api/manifest/diff", &body));
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(resp.body.contains("\"changed\":[{\"name\":\"nested/x.bin\""), "{}", resp.body);
        assert!(resp.body.contains("\"only_b\":[{\"name\":\"new.txt\""), "{}", resp.body);
        assert!(resp.body.contains("\"only_a\":["), "{}", resp.body);

        // Paths outside the engagement tree are refused.
        let outside = handle(&st, &Req::get("/api/manifest?path=/"));
        assert!(
            outside.status == 403 || outside.status == 404,
            "{} {}",
            outside.status,
            outside.body
        );
    }

    // -- operations ----------------------------------------------------------

    fn entry_body() -> String {
        "{\"ref\":\"AJE 1\",\"date\":\"20261231\",\"desc\":\"web entry\",\"type\":\"N\",\
\"period\":12,\"lines\":[{\"acct\":\"1000\",\"amount\":100.0},{\"acct\":\"4000\",\"amount\":-100.0}]}"
            .to_string()
    }

    #[test]
    fn entry_preview_writes_nothing_and_commit_does() {
        let (dir, st) = state_for("entries-op");
        let before_gl = std::fs::read(dir.join("SYNTHGL.dbf")).unwrap();
        let before_bl = std::fs::read(dir.join("SYNTHBL.dbf")).unwrap();

        let resp = handle(&st, &Req::post("/api/entries/preview", &entry_body()));
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(resp.body.contains("\"ok\":true"), "{}", resp.body);
        assert!(resp.body.contains("\"op\":\"entries\""), "{}", resp.body);
        assert!(resp.body.contains("\"preview\":{"), "{}", resp.body);
        assert!(resp.body.contains("\"balanced\":true"), "{}", resp.body);
        assert!(resp.body.contains("\"manifest\":{"), "{}", resp.body);
        assert!(
            resp.body.contains("{\"name\":\"SYNTHGL.dbf\""),
            "GL must be listed as changed: {}",
            resp.body
        );
        assert_eq!(
            std::fs::read(dir.join("SYNTHGL.dbf")).unwrap(),
            before_gl,
            "preview must not touch GL"
        );
        assert_eq!(
            std::fs::read(dir.join("SYNTHBL.dbf")).unwrap(),
            before_bl,
            "preview must not touch BL"
        );
        assert!(st.work.exists(), "work area created on demand");
        let strays: Vec<String> = std::fs::read_dir(&st.work)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(strays.is_empty(), "preview scratch removed: {strays:?}");

        let resp = handle(&st, &Req::post("/api/entries/commit", &entry_body()));
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(resp.body.contains("\"result\":{"), "{}", resp.body);
        assert!(resp.body.contains("\"backup\":"), "{}", resp.body);
        assert_ne!(
            std::fs::read(dir.join("SYNTHGL.dbf")).unwrap(),
            before_gl,
            "commit writes GL"
        );
        assert_ne!(
            std::fs::read(dir.join("SYNTHBL.dbf")).unwrap(),
            before_bl,
            "commit recalcs BL (F35)"
        );
        // AM is never touched by an entry write.
        let gl = Table::parse_path(dir.join("SYNTHGL.dbf")).unwrap();
        let journal = Adjustments::from_table(&gl).unwrap();
        assert_eq!(journal.len(), 1);
        assert_eq!(journal.entries()[0].system_id, "AJE 1");
    }

    #[test]
    fn entry_refusals_are_structured() {
        let (_dir, st) = state_for("entries-refuse");
        let unbalanced = "{\"ref\":\"AJE 2\",\"date\":\"20261231\",\"lines\":[{\"acct\":\"1000\",\"amount\":100}]}";
        let resp = handle(&st, &Req::post("/api/entries/preview", unbalanced));
        assert_eq!(resp.status, 422, "{}", resp.body);
        assert!(resp.body.contains("\"error\":{"), "{}", resp.body);

        let unknown = "{\"ref\":\"AJE 3\",\"date\":\"20261231\",\"lines\":[{\"acct\":\"NOPE\",\"amount\":1},{\"acct\":\"1000\",\"amount\":-1}]}";
        let resp = handle(&st, &Req::post("/api/entries/preview", unknown));
        assert_eq!(resp.status, 422);
        assert!(resp.body.contains("unknown_account"), "{}", resp.body);

        let bad_json = handle(&st, &Req::post("/api/entries/preview", "{not json"));
        assert_eq!(bad_json.status, 400);
        assert!(bad_json.body.contains("invalid JSON body"));
    }

    #[test]
    fn renumber_preview_reports_the_cascade_and_commit_writes() {
        let (dir, st) = state_for("renumber");
        let am_before = std::fs::read(dir.join("SYNTHAM.dbf")).unwrap();
        let body = "{\"delta\":100}";
        let resp = handle(&st, &Req::post("/api/renumber/preview", body));
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(resp.body.contains("\"renames\":[{\"from\":\"1000\",\"to\":\"1100\"}"), "{}", resp.body);
        assert!(resp.body.contains("\"cells\":"), "{}", resp.body);
        assert_eq!(
            std::fs::read(dir.join("SYNTHAM.dbf")).unwrap(),
            am_before,
            "renumber preview writes nothing"
        );

        let resp = handle(&st, &Req::post("/api/renumber/commit", body));
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert_ne!(
            std::fs::read(dir.join("SYNTHAM.dbf")).unwrap(),
            am_before,
            "renumber commit rewrites AM"
        );
        let chart = Chart::from_table(&Table::parse_path(dir.join("SYNTHAM.dbf")).unwrap()).unwrap();
        let numbers: Vec<String> = chart.posting().map(|a| a.ac_no.trim().to_string()).collect();
        assert!(numbers.contains(&"1100".to_string()), "{numbers:?}");
    }

    #[test]
    fn renumber_refusal_carries_the_issues() {
        let (dir, st) = state_for("renumber-refuse");
        let before = std::fs::read(dir.join("SYNTHAM.dbf")).unwrap();
        let resp = handle(
            &st,
            &Req::post(
                "/api/renumber/preview",
                "{\"plan\":[{\"from\":\"9999\",\"to\":\"8888\"}]}",
            ),
        );
        assert_eq!(resp.status, 409, "{}", resp.body);
        assert!(resp.body.contains("\"code\":\"cascade_refused\""), "{}", resp.body);
        assert!(resp.body.contains("\"preview\":{"), "{}", resp.body);
        assert_eq!(
            std::fs::read(dir.join("SYNTHAM.dbf")).unwrap(),
            before,
            "a refused cascade writes nothing"
        );
    }

    #[test]
    fn yearend_preview_projects_and_commit_builds_a_new_copy() {
        let (_dir, st) = state_for("yearend");
        let body = "{\"retained_earning\":\"4000\"}";
        let resp = handle(&st, &Req::post("/api/yearend/preview", body));
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(resp.body.contains("\"rows\":["), "{}", resp.body);
        assert!(resp.body.contains("\"key_totals\":{"), "{}", resp.body);
        assert!(resp.body.contains("\"destination\":"), "{}", resp.body);
        assert!(
            resp.body.contains("BL.dbf\""),
            "preview lists the projected files: {}",
            resp.body
        );
        assert!(
            resp.body.contains("\"only_b\":["),
            "preview manifests the files it would create: {}",
            resp.body
        );

        let resp = handle(&st, &Req::post("/api/yearend/commit", body));
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(resp.body.contains("\"ok\":true"), "{}", resp.body);
        assert!(resp.body.contains("\"backup\":"), "{}", resp.body);
        let created: Vec<String> = std::fs::read_dir(&st.work)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("yearend") && !n.contains("preview"))
            .collect();
        assert_eq!(created.len(), 1, "one new destination: {created:?}");
        // The close renames the stem, so the destination carries new tables.
        let dest = st.work.join(&created[0]);
        let tables: Vec<String> = std::fs::read_dir(&dest)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.to_ascii_uppercase().ends_with("BL.DBF"))
            .collect();
        assert_eq!(tables.len(), 1, "destination BL written: {tables:?}");
    }

    #[test]
    fn yearend_requires_a_retained_earnings_account() {
        let (dir, _st) = state_for("yearend-re");
        // Strip every description naming retained earnings, then ask for a
        // close without naming one.
        let mut am = Table::parse_path(dir.join("SYNTHAM.dbf")).unwrap();
        for row in 0..am.records.len() {
            put(&mut am, row, "AC_DESC", "plain");
        }
        std::fs::write(dir.join("SYNTHAM.dbf"), am.encode()).unwrap();
        let st = State::open(&dir).unwrap();
        let resp = handle(&st, &Req::post("/api/yearend/preview", "{}"));
        assert_eq!(resp.status, 422, "{}", resp.body);
        assert!(resp.body.contains("retained_earning_required"), "{}", resp.body);
    }

    #[test]
    fn lifecycle_backup_previews_and_commits() {
        let (_dir, st) = state_for("backup");
        let resp = handle(
            &st,
            &Req::post("/api/lifecycle/backup/preview", "{\"path\":\"\"}"),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(resp.body.contains("\"op\":\"backup\""), "{}", resp.body);
        assert!(resp.body.contains("\"only_b\":["), "{}", resp.body);

        let resp = handle(
            &st,
            &Req::post("/api/lifecycle/backup/commit", "{\"path\":\"\"}"),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(resp.body.contains("\"result\":{"), "{}", resp.body);
    }

    /// The client's alternate shape (`mode` in the body, no phase segment).
    #[test]
    fn lifecycle_accepts_mode_in_the_body() {
        let (_dir, st) = state_for("lifecycle-mode");
        let resp = handle(
            &st,
            &Req::post("/api/lifecycle/backup", "{\"mode\":\"preview\",\"path\":\"\"}"),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(resp.body.contains("\"only_b\":["), "{}", resp.body);
        let unknown = handle(&st, &Req::post("/api/lifecycle/nope", "{\"mode\":\"preview\"}"));
        assert_eq!(unknown.status, 404);
    }

    #[test]
    fn lifecycle_compress_round_trips_a_package() {
        let (_dir, st) = state_for("compress");
        let resp = handle(
            &st,
            &Req::post("/api/lifecycle/compress/preview", "{\"path\":\"\"}"),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(resp.body.contains("\"op\":\"compress\""), "{}", resp.body);
        assert!(
            resp.body.contains("SYNTH.ac_"),
            "the preview names the package: {}",
            resp.body
        );

        let resp = handle(
            &st,
            &Req::post("/api/lifecycle/compress/commit", "{\"path\":\"\"}"),
        );
        if resp.status != 200 {
            // `package_engagement` needs the `.ac` hub; a synthetic engagement
            // without one is a legitimate refusal, not a server fault.
            assert!(resp.body.contains("\"error\":{"), "{}", resp.body);
            return;
        }
        assert!(resp.body.contains("\"result\":{"), "{}", resp.body);
    }

    // -- document manager (DMS) ---------------------------------------------

    /// A real `.cvw` with a cell directory, copied into a fixture folder.
    ///
    /// `CfbBuilder` writes flat stream names, while CaseView nests its streams
    /// under `Form`/`Index` storages, so a renderable CaseView container cannot
    /// be synthesised here. The staged documents are used when present; the
    /// tests that need one skip cleanly otherwise.
    fn staged_cvw() -> Option<Vec<u8>> {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cands = [
            repo.join("handover/12312024 PPC USA Inc Review(1256259)01200RG0.cvw"),
            repo.join("expanded/2024_Hull_Lift_Truck_Review(1148441)/2024 Hull Lift Truck Review(1148441)01200RG0.cvw"),
        ];
        cands.iter().find(|p| p.is_file()).map(|p| std::fs::read(p).expect("staged cvw"))
    }

    /// A fixture folder: an engagement plus a document set covering every class
    /// and every join tier.
    fn docman_fixture(tag: &str) -> (PathBuf, State) {
        let dir = blank_engagement(tag);
        let stem = "SYNTH";
        // Documents on disk.
        std::fs::write(dir.join("0102_Review.docx"), b"DOCX").unwrap();
        std::fs::write(dir.join("0102_Review-2.xlsx"), b"XLSX").unwrap();
        std::fs::write(dir.join("Summary.pdf"), b"%PDF-1.4 summary").unwrap();
        std::fs::write(dir.join("notes.bmp"), b"BM00").unwrap();
        std::fs::write(dir.join("loose.xls"), b"XLS").unwrap();
        // A CVW whose filename tail is the CVID (when a real one is staged).
        let staged = staged_cvw();
        if let Some(bytes) = &staged {
            std::fs::write(dir.join(format!("{stem}01200RG0.cvw")), bytes).unwrap();
        } else {
            // Not renderable, but still a physical file with a CVID tail.
            let mut bare = cw_core::cfb::CfbBuilder::new();
            bare.add("Form/Layout", vec![0u8; 128]);
            std::fs::write(dir.join(format!("{stem}01200RG0.cvw")), bare.build().unwrap()).unwrap();
        }
        // A CVW that cannot be laid out (no cell directory at all).
        let mut bare = cw_core::cfb::CfbBuilder::new();
        bare.add("Form/Layout", vec![0u8; 128]);
        bare.add("Form/Styles", vec![0u8; 32]);
        std::fs::write(dir.join(format!("{stem}empty.cvw")), bare.build().unwrap()).unwrap();
        // OS noise and package transients: never documents.
        std::fs::write(dir.join(".DS_Store"), b"noise").unwrap();
        std::fs::write(dir.join("._0102_Review.docx"), b"fork").unwrap();
        std::fs::write(dir.join(format!("{stem}.ac_")), b"transient").unwrap();
        // A subfolder document (the walk is recursive).
        std::fs::create_dir_all(dir.join("PBC")).unwrap();
        std::fs::write(dir.join("PBC/Support.xlsx"), b"XLSX2").unwrap();

        // Register rows: one form joined by CVID, one by schedule prefix, one
        // that is register-only, and one heading that must never be a key.
        if let Some(fields) = sh_fields_from_corpus() {
            let mut sh = blank_table(&fields, 4);
            // 0: 0120 REV form, CVID 01200RG0, signed at level 1.
            put(&mut sh, 0, "LINENO", "100");
            put(&mut sh, 0, "PRG", "ac");
            put(&mut sh, 0, "SCHEDULE", "0120 REV");
            put(&mut sh, 0, "TYPE", "F");
            put(&mut sh, 0, "SCH_DESC", "Review Engagement Completion Questionnaire");
            put(&mut sh, 0, "CVID", "01200RG0");
            put(&mut sh, 0, "CWGUID", "GUIDFORM0000000000001");
            put(&mut sh, 0, "A1", "AB");
            put(&mut sh, 0, "AD1", "20240517");
            // 1: 0102 REV index row (matched by the schedule prefix).
            put(&mut sh, 1, "LINENO", "101");
            put(&mut sh, 1, "PRG", "ac");
            put(&mut sh, 1, "SCHEDULE", "0102 REV");
            put(&mut sh, 1, "TYPE", "*");
            put(&mut sh, 1, "SCH_DESC", "Report Release and Delivery Record");
            put(&mut sh, 1, "CWGUID", "GUIDINDEX0000000000001");
            // 2: a heading whose schedule is a GUID (never a join key).
            put(&mut sh, 2, "PRG", "ac");
            put(&mut sh, 2, "SCHEDULE", "9B70BC01-5573-45E6-B76B-D4FCA2508A5F");
            put(&mut sh, 2, "TYPE", "H");
            put(&mut sh, 2, "SCH_DESC", "0200 Account and Reporting Workpapers");
            put(&mut sh, 2, "CWGUID", "GUIDHEAD0000000000001");
            // 3: an analysis with no file on disk (register-only).
            put(&mut sh, 3, "LINENO", "102");
            put(&mut sh, 3, "PRG", "ac");
            put(&mut sh, 3, "SCHEDULE", "9999");
            put(&mut sh, 3, "TYPE", "A");
            put(&mut sh, 3, "NAME", "TB");
            put(&mut sh, 3, "SCH_DESC", "Trial balance Report");
            put(&mut sh, 3, "CWGUID", "GUIDANAL0000000000001");
            std::fs::write(dir.join(format!("{stem}SH.dbf")), sh.encode()).unwrap();
        }
        let st = State::open(&dir).expect("state opens");
        (dir, st)
    }

    #[test]
    fn docman_lists_every_file_and_skips_os_noise() {
        let (_dir, st) = docman_fixture("dms-list");
        let resp = handle(&st, &Req::get("/api/docman?internals=1"));
        assert_eq!(resp.status, 200, "{}", resp.body);
        for want in [
            "\"0102_Review.docx\"",
            "\"0102_Review-2.xlsx\"",
            "\"Summary.pdf\"",
            "\"notes.bmp\"",
            "\"SYNTH01200RG0.cvw\"",
            "\"PBC/Support.xlsx\"",
        ] {
            assert!(resp.body.contains(want), "{want} missing from {}", resp.body);
        }
        for never in [".DS_Store", "._0102_Review.docx", "SYNTH.ac_"] {
            assert!(!resp.body.contains(never), "{never} must be skipped");
        }
    }

    #[test]
    fn docman_class_and_internals_filters() {
        let (_dir, st) = docman_fixture("dms-filter");
        let pdf = handle(&st, &Req::get("/api/docman?class=pdf"));
        assert_eq!(pdf.status, 200, "{}", pdf.body);
        assert!(pdf.body.contains("\"Summary.pdf\""));
        assert!(!pdf.body.contains("\"0102_Review.docx\""));

        // Internals are off by default and on request they appear (the SH
        // kernel tables and the hub).
        let plain = handle(&st, &Req::get("/api/docman"));
        assert!(!plain.body.contains("\"SYNTHSH.dbf\""));
        let all = handle(&st, &Req::get("/api/docman?internals=1"));
        assert!(all.body.contains("\"SYNTHSH.dbf\""));
        assert!(all.body.contains("\"SYNTH.ac\""));

        let none = handle(&st, &Req::get("/api/docman?q=nothing-matches-this"));
        assert!(none.body.contains("\"rows\":[]"), "{}", none.body);
    }

    #[test]
    fn docman_join_uses_cvid_schedule_and_reports_misses() {
        let (_dir, st) = docman_fixture("dms-join");
        let resp = handle(&st, &Req::get("/api/docman"));
        assert_eq!(resp.status, 200, "{}", resp.body);
        // CVW joined by CVID.
        assert!(
            resp.body.contains("\"tier\":\"cvid\"") || resp.body.contains("\"cvid\":"),
            "{}",
            resp.body
        );
        // The register-only analysis is listed with no file behind it.
        assert!(resp.body.contains("\"backing\":\"register\""), "{}", resp.body);
        assert!(resp.body.contains("Trial balance Report"), "{}", resp.body);
        // The heading's GUID-shaped SCHEDULE never becomes a join key: no
        // *file* row claims it (the heading is still listed as a register row).
        for row in resp.body.split("{\"id\":").skip(1) {
            if !row.starts_with("\"file:") || !row.contains("\"backing\":\"file\"") {
                continue;
            }
            let end = row.find("},{\"id\"").unwrap_or(row.len());
            assert!(
                !row[..end].contains("9B70BC01-5573-45E6-B76B-D4FCA2508A5F"),
                "a heading row was used as a file's register match: {}",
                &row[..end]
            );
        }
        assert!(resp.body.contains("\"backing\":\"register\""), "{}", resp.body);
        // And the heading itself is present, once, as a register row.
        assert!(resp.body.contains("\"title\":\"0200 Account and Reporting Workpapers\""));
        // The join law is published with real hits.
        assert!(resp.body.contains("\"join\":{\"tiers\":["), "{}", resp.body);
        assert!(resp.body.contains("\"misses\":["), "{}", resp.body);
    }

    #[test]
    fn docman_file_route_serves_raw_bytes_with_mime() {
        let (_dir, st) = docman_fixture("dms-file");
        let resp = handle(&st, &Req::get("/api/docman/file?path=Summary.pdf"));
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert_eq!(resp.content_type, "application/pdf");
        assert_eq!(resp.raw.as_deref(), Some(&b"%PDF-1.4 summary"[..]));
        assert!(resp
            .disposition
            .as_deref()
            .unwrap_or("")
            .starts_with("inline"));

        let docx = handle(&st, &Req::get("/api/docman/file?path=0102_Review.docx"));
        assert_eq!(docx.content_type, "application/vnd.openxmlformats-officedocument.wordprocessingml.document");

        // The guard refuses absolute paths, traversal and directories.
        assert_eq!(
            handle(&st, &Req::get("/api/docman/file?path=/etc/hosts")).status,
            400
        );
        assert_eq!(
            handle(&st, &Req::get("/api/docman/file?path=../../etc/hosts")).status,
            404
        );
        assert_eq!(
            handle(&st, &Req::get("/api/docman/file?path=PBC")).status,
            400
        );
        assert_eq!(handle(&st, &Req::get("/api/docman/file")).status, 400);
    }

    #[test]
    fn docman_cvw_preview_renders_structure_and_never_invents_text() {
        let (_dir, st) = docman_fixture("dms-cvw");
        if staged_cvw().is_none() {
            eprintln!("no staged .cvw; skipping the render assertions");
        } else {
            let json = handle(
                &st,
                &Req::get("/api/docman/preview-cvw?path=SYNTH01200RG0.cvw&format=json"),
            );
            assert_eq!(json.status, 200, "{}", json.body);
            assert!(json.body.contains("\"rendered\":true"), "{}", json.body);
            assert!(json.body.contains("\"cells\":376"), "{}", json.body);
            // The hull document carries no `Form/Strings` pool: the preview
            // must say so and bind no text rather than invent it.
            if json.body.contains("\"text_bound\":0") {
                assert!(
                    json.body.contains("cell text unavailable"),
                    "the preview must say why it has no text: {}",
                    json.body
                );
            }
            let html = handle(
                &st,
                &Req::get("/api/docman/preview-cvw?path=SYNTH01200RG0.cvw"),
            );
            assert_eq!(html.status, 200);
            assert_eq!(html.content_type, CT_HTML);
            assert!(html.body.contains("<table class=\"sheet\">"), "a sheet is laid out");
            assert!(html.body.contains("data-cell=\""), "cells are addressable");
            assert!(!html.body.contains("http://"), "preview must be airgapped");
            assert!(!html.body.contains("https://"), "preview must be airgapped");
        }

        // A document with no cell directory is reported, not fabricated.
        let bare = handle(
            &st,
            &Req::get("/api/docman/preview-cvw?path=SYNTHempty.cvw&format=json"),
        );
        assert_eq!(bare.status, 200, "{}", bare.body);
        assert!(bare.body.contains("\"rendered\":false"), "{}", bare.body);
        assert!(bare.body.contains("no cell directory"), "{}", bare.body);
        assert!(bare.body.contains("\"blocks_detail\":[]"), "{}", bare.body);

        let bare_html = handle(&st, &Req::get("/api/docman/preview-cvw?path=SYNTHempty.cvw"));
        assert!(bare_html.body.contains("data-render=\"failed\""), "{}", bare_html.body);
        assert!(bare_html.body.contains("fabricated"), "{}", bare_html.body);

        // A non-CaseView file is refused.
        let wrong = handle(
            &st,
            &Req::get("/api/docman/preview-cvw?path=0102_Review.docx"),
        );
        assert_eq!(wrong.status, 400);
    }

    #[test]
    fn docman_repairs_a_missing_eof_and_a_garbage_tail() {
        let (dir, _st) = docman_fixture("dms-repair");
        let Some(fields) = sh_fields_from_corpus() else {
            return;
        };
        let stem = "SYNTH";
        let mut sh = blank_table(&fields, 4);
        put(&mut sh, 0, "SCHEDULE", "0102");
        put(&mut sh, 0, "TYPE", "*");
        put(&mut sh, 0, "SCH_DESC", "Report Release and Delivery Record");
        put(&mut sh, 0, "SCHEDULE", "0102");
        put(&mut sh, 1, "SCHEDULE", "0103");
        put(&mut sh, 1, "TYPE", "*");
        put(&mut sh, 1, "SCH_DESC", "Second document");
        // Rows 2 and 3 are zero-fill synthesis with an invalid delete flag.
        for r in 2..4 {
            for b in &mut sh.records[r] {
                *b = 0;
            }
        }
        let table = sh.encode();
        // Strip the 0x1A terminator (the expanded hull corpus shape).
        let mut bytes = table[..table.len() - 1].to_vec();
        std::fs::write(dir.join(format!("{stem}SH.dbf")), &bytes).unwrap();

        let st = State::open(&dir).expect("state opens");
        let resp = handle(&st, &Req::get("/api/docman"));
        assert_eq!(resp.status, 200, "{}", resp.body);
        // Declared 4, but the readable prefix is 2 and the repair says so.
        assert!(resp.body.contains("\"declared\":4"), "{}", resp.body);
        assert!(resp.body.contains("\"live\":2"), "{}", resp.body);
        assert!(resp.body.contains("\"repaired\":true"), "{}", resp.body);
        assert!(resp.body.contains("readable frontier"), "{}", resp.body);

        // The register lens (`/api/docs`) reads the same repaired table.
        let census = handle(&st, &Req::get("/api/docs?mode=census"));
        assert_eq!(census.status, 200, "{}", census.body);
        assert!(census.body.contains("\"live\":2"), "{}", census.body);

        // Restore the terminator so the table is well-formed again.
        bytes.push(0x1A);
        std::fs::write(dir.join(format!("{stem}SH.dbf")), &bytes).unwrap();
    }

    #[test]
    fn docman_route_is_bound_in_the_contract_table() {
        let (_dir, st) = state_for("dms-routes");
        for (req, want) in [
            (Req::get("/api/docman"), 200u16),
            (Req::get("/api/docman?internals=1&class=pdf&q=x&sha=0"), 200),
            (Req::get("/api/docman/file"), 400),
            (Req::get("/api/docman/preview-cvw"), 400),
        ] {
            let resp = handle(&st, &req);
            assert_eq!(resp.status, want, "{} -> {}: {}", req.path, resp.status, resp.body);
        }
    }

    /// The real engagement folder: 178 files, the SH join where it exists, and
    /// a `.cvw` that lays out. Skipped when the expanded corpus is absent.
    #[test]
    fn corpus_document_manager_lists_the_real_file_set() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../expanded/2024_Hull_Lift_Truck_Review(1148441)");
        if !dir.is_dir() {
            eprintln!("expanded hull corpus not staged; skipping");
            return;
        }
        let st = State::open(&dir).expect("real engagement opens");
        let resp = handle(&st, &Req::get("/api/docman"));
        assert_eq!(resp.status, 200, "{}", resp.body);
        let body = &resp.body;
        assert!(body.contains("\"files\":178"), "{}", &body[..body.len().min(400)]);
        assert!(body.contains("\"joined\":35"), "joined rows");
        assert!(body.contains("\"0102_Review.docx\""), "unindexed file present");
        // Every live SH row is accounted for exactly once: joined or register-only.
        assert!(body.contains("\"register_only\":71"), "register-only rows counted");
        assert!(body.contains("\"unindexed\":82"), "unindexed files counted");
        assert!(
            body.contains("\"tier\":\"cvid\""),
            "CVW files join their form row by CVID"
        );

        // The form whose strings pool survives renders text; the others state
        // the absence. Either way the route answers with a sheet or a reason.
        let p = handle(
            &st,
            &Req::get(
                "/api/docman/preview-cvw?path=2024%20Hull%20Lift%20Truck%20Review(1148441)01200RG0.cvw&format=json",
            ),
        );
        assert_eq!(p.status, 200, "{}", p.body);
        assert!(p.body.contains("\"rendered\":true"), "{}", p.body);
        assert!(p.body.contains("\"kind\":\"statement\""), "{}", p.body);
    }

    // -- static assets -------------------------------------------------------

    #[test]
    fn embedded_assets_are_served_verbatim() {
        let (_dir, st) = state_for("static");
        assert_eq!(handle(&st, &Req::get("/")).body, INDEX_HTML);
        assert_eq!(handle(&st, &Req::get("/static/app.js")).body, APP_JS);
        assert_eq!(handle(&st, &Req::get("/static/style.css")).body, STYLE_CSS);
        assert!(INDEX_HTML.contains("id=\"app\""), "the shell mounts #app");
        assert!(APP_JS.contains("/api/overview"), "app.js talks to the API");
        assert!(!APP_JS.contains("http://"), "airgap: no external origin in app.js");
    }

    // -- corpus gate (skipped when the frozen corpus is absent) --------------

    #[test]
    fn corpus_surfaces_serve_end_to_end() {
        let dir = golden("corp2026");
        if !dir.is_dir() {
            return;
        }
        let st = match State::open(&dir) {
            Ok(s) => s,
            Err(e) => panic!("corpus must open: {e}"),
        };
        let overview = handle(&st, &Req::get("/api/overview"));
        assert_eq!(overview.status, 200);
        assert!(overview.body.contains("\"am_posting\":"), "{}", overview.body);

        let tb = handle(&st, &Req::get("/api/tb?year=0&period=12&group=A&limit=0"));
        assert_eq!(tb.status, 200, "{}", tb.body);
        assert!(tb.body.contains("\"rows\":[{\"acct\":\""), "{}", tb.body);

        let census = handle(&st, &Req::get("/api/docs?mode=census"));
        assert_eq!(census.status, 200, "{}", census.body);
        assert!(census.body.contains("\"live\":"), "{}", census.body);

        // A real document: the first live SH guid, signed or not.
        let book = crate::docman::open_book(&dir, &st.eng.stem).expect("SH decodes");
        let doc = book
            .documents()
            .iter()
            .find(|d| !d.cwguid.is_empty())
            .expect("a live document");
        let resp = handle(&st, &Req::get(&format!("/api/signoff?doc={}", doc.cwguid)));
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(resp.body.contains("\"levels\":["), "{}", resp.body);
        assert_eq!(resp.body.matches("\"level\":").count(), 8, "{}", resp.body);

        // Entries: the corpus journal decodes into the grouped view.
        let entries = handle(&st, &Req::get("/api/entries"));
        assert_eq!(entries.status, 200, "{}", entries.body);
        assert!(entries.body.contains("\"type_label\":"), "{}", entries.body);

        // The account route on a real posting account.
        let chart = Chart::from_table(&Table::parse_path(
            std::fs::read_dir(&dir)
                .unwrap()
                .flatten()
                .map(|e| e.path())
                .find(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().to_ascii_uppercase().ends_with("AM.DBF"))
                        .unwrap_or(false)
                })
                .expect("AM table"),
        )
        .unwrap())
        .expect("chart");
        let ac_no = chart.posting().next().expect("posting account").ac_no.trim().to_string();
        let resp = handle(
            &st,
            &Req::get(&format!("/api/account/{}", pct_encode(&ac_no))),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
    }

    fn pct_encode(s: &str) -> String {
        let mut out = String::new();
        for b in s.as_bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(*b as char)
                }
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }

    // -- manifest helpers ----------------------------------------------------

    #[test]
    fn manifest_diff_classifies_all_three_buckets() {
        let a = vec![
            FileEntry {
                name: "same".into(),
                bytes: 1,
                sha256: "AA".into(),
            },
            FileEntry {
                name: "gone".into(),
                bytes: 1,
                sha256: "BB".into(),
            },
            FileEntry {
                name: "edit".into(),
                bytes: 1,
                sha256: "CC".into(),
            },
        ];
        let b = vec![
            FileEntry {
                name: "same".into(),
                bytes: 1,
                sha256: "AA".into(),
            },
            FileEntry {
                name: "edit".into(),
                bytes: 2,
                sha256: "DD".into(),
            },
            FileEntry {
                name: "fresh".into(),
                bytes: 3,
                sha256: "EE".into(),
            },
        ];
        let d = diff_manifests(&a, &b);
        assert_eq!(d.only_a.len(), 1);
        assert_eq!(d.only_a[0].name, "gone");
        assert_eq!(d.only_b.len(), 1);
        assert_eq!(d.only_b[0].name, "fresh");
        assert_eq!(d.changed.len(), 1);
        assert_eq!(d.changed[0].name, "edit");
        assert!(d.render().contains("\"only_a\":[{\"name\":\"gone\""));
    }

    #[test]
    fn multi_root_manifests_are_prefixed() {
        let one = temp_dir("roots-a");
        let two = temp_dir("roots-b");
        std::fs::write(one.join("x"), b"1").unwrap();
        std::fs::write(two.join("x"), b"2").unwrap();
        let files = manifest_roots(&[
            ("engagement".to_string(), one.clone()),
            ("work".to_string(), two.clone()),
        ])
        .unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["engagement/x", "work/x"]);
        let _ = std::fs::remove_dir_all(&one);
        let _ = std::fs::remove_dir_all(&two);
    }
}
