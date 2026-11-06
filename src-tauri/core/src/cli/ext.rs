//! `c9watch ext check <dir>` (glass build plan G11, D35a): checks one
//! extension directory before it is shared or placed in an extensions root.
//!
//! The manifest rules are the GUI host's (`src/shells/glass/ext/manifest.ts`,
//! `parseManifest`, owner D41 R1–R12). This file must give the same answer
//! and the same reason for every `c9ext.json`; a test runs the same cases.
//! JavaScript semantics that differ from Rust are copied on purpose: string
//! lengths count UTF-16 units, `trim()` uses the ECMAScript white space set,
//! and a JSON number is compared as a double (`1.0 === 1`).
//! Known limitation: serde_json rejects a few inputs JSON.parse accepts (a
//! number out of f64 range such as `1e400`, a lone surrogate escape, nesting
//! deeper than 128), so the check calls such a manifest invalid JSON.
//!
//! The file rules are the GUI crate's `c9ext://` server (`src-tauri/src/ext.rs`):
//! a page entry must be a real file inside the extension directory after
//! symlinks are resolved, and the server must be willing to send it. The
//! check also needs `RECIPE.md` and `preview.png` (D35a packaging).
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Serialize;
use serde_json::Value;

/// The manifest file name.
pub const MANIFEST: &str = "c9ext.json";
/// The packaging files every extension directory carries (D35a).
pub const RECIPE: &str = "RECIPE.md";
pub const PREVIEW: &str = "preview.png";
/// The host's `UI_CONTRACT`, `MAX_MANIFEST_BYTES`, `MAX_PAGES`, `MAX_NAME`.
pub const UI_CONTRACT: &str = "c9ui/1";
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
pub const MAX_PAGES: usize = 16;
pub const MAX_NAME: usize = 64;
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

#[derive(clap::Subcommand, Debug)]
pub enum ExtAction {
    /// Check one extension directory: manifest, entry files, RECIPE.md and
    /// preview.png. Exit codes: 0 pass, 1 fail
    Check {
        /// The extension directory. Its name must equal the manifest `id`
        dir: PathBuf,
        /// Print the result as one JSON object instead of text
        #[arg(long)]
        json: bool,
    },
}

/// Runs `c9watch ext <action>` and exits with its code.
pub fn run(action: ExtAction, pretty: bool) -> Result<(), String> {
    match action {
        ExtAction::Check { dir, json } => {
            let report = check(&dir);
            if json {
                super::print_json(&report, pretty)?;
            } else {
                print!("{}", report.text());
            }
            if !report.ok {
                std::process::exit(1);
            }
            Ok(())
        }
    }
}

// ── The manifest (mirror of manifest.ts) ────────────────────────────

/// One `ui.pages[]` item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Page {
    pub id: String,
    pub title: String,
    pub entry: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// `minVersion`. `apiMinor` is kept as the JSON double the host compares.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct MinVersion {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(rename = "apiMinor", skip_serializing_if = "Option::is_none")]
    pub api_minor: Option<f64>,
}

/// A valid manifest, as the host's `Manifest`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(rename = "minVersion", skip_serializing_if = "Option::is_none")]
    pub min_version: Option<MinVersion>,
    /// `ui.pages`; None when there is no `ui` (R3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<Vec<Page>>,
    pub dev: bool,
}

/// The host's `Parsed` when `ok` is true.
#[derive(Clone, Debug, PartialEq)]
pub struct Parsed {
    pub manifest: Manifest,
    pub has_core: bool,
    pub warnings: Vec<String>,
}

/// The host's `ID_RE`, `^[a-z][a-z0-9-]{0,31}$`.
pub fn valid_id(id: &str) -> bool {
    let b = id.as_bytes();
    (1..=32).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// The host's `SEMVER_RE` (semver.org 2.0.0). `\d` is spelled `[0-9]`:
/// a Rust `\d` matches every Unicode digit, a JavaScript `\d` only ASCII.
fn semver_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(concat!(
            r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)",
            r"(?:-((?:0|[1-9][0-9]*|[0-9]*[a-zA-Z-][0-9a-zA-Z-]*)",
            r"(?:\.(?:0|[1-9][0-9]*|[0-9]*[a-zA-Z-][0-9a-zA-Z-]*))*))?",
            r"(?:\+([0-9a-zA-Z-]+(?:\.[0-9a-zA-Z-]+)*))?$"
        ))
        .expect("semver regex")
    })
}

/// The host's `isSemver`.
pub fn is_semver(v: &Value) -> bool {
    v.as_str()
        .is_some_and(|s| js_len(s) <= 256 && semver_re().is_match(s))
}

/// `String.prototype.length`: UTF-16 code units.
fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// ECMAScript WhiteSpace and LineTerminator, the set `trim()` removes.
/// Rust's `char::is_whitespace` differs: it has U+0085 and lacks U+FEFF.
fn js_space(c: char) -> bool {
    matches!(
        c,
        '\u{9}'..='\u{d}'
            | ' '
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// `!s.trim()`: empty after JavaScript's trim.
fn js_blank(s: &str) -> bool {
    s.chars().all(js_space)
}

/// A string of 1..=max UTF-16 units that is not all white space.
fn text_field(v: Option<&Value>, max: usize) -> Option<&str> {
    v.and_then(Value::as_str)
        .filter(|s| !js_blank(s) && js_len(s) <= max)
}

/// `x === 1` for a parsed JSON value: a number whose double is 1.
fn is_one(v: Option<&Value>) -> bool {
    v.and_then(Value::as_f64) == Some(1.0)
}

/// `Number.isInteger(x) && x >= 0`.
fn non_negative_integer(v: &Value) -> Option<f64> {
    v.as_f64()
        .filter(|n| n.is_finite() && n.fract() == 0.0 && *n >= 0.0)
}

/// The host's `isEntryPath`: a relative path inside the extension directory,
/// with no `\`, `%`, `:`, `?` or `#`, and no empty, `.` or `..` segment.
fn is_entry_path(v: Option<&Value>) -> Option<&str> {
    let s = v?.as_str()?;
    if s.is_empty() || js_len(s) > 200 || s.starts_with('/') {
        return None;
    }
    if s.contains(['\\', '%', ':', '?', '#']) {
        return None;
    }
    s.split('/')
        .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
        .then_some(s)
}

/// The host's `coreWarnings` (R12). Loose: only warnings, never an error.
fn core_warnings(core: &Value) -> Vec<String> {
    let Some(core) = core.as_object() else {
        return Vec::new();
    };
    let Some(events) = core.get("events") else {
        return Vec::new();
    };
    let Some(events) = events.as_array() else {
        return vec!["core.events 不是陣列".into()];
    };
    let mut out = Vec::new();
    for (i, ev) in events.iter().enumerate() {
        let ok = match ev {
            Value::String(s) => !s.is_empty(),
            Value::Object(o) => {
                o.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| !n.is_empty())
                    && match o.get("subscribers") {
                        None => true,
                        Some(Value::Array(a)) => a.iter().all(Value::is_string),
                        Some(_) => false,
                    }
            }
            _ => false,
        };
        if !ok {
            out.push(format!("core.events[{i}] 格式不對"));
        }
    }
    out
}

/// The host's `parseManifest(text, dir, size)`. Same order, same reasons.
pub fn parse_manifest(text: Option<&str>, dir: &str, size: u64) -> Result<Parsed, String> {
    let Some(text) = text else {
        return Err(if size > MAX_MANIFEST_BYTES {
            "manifest 超過 64 KiB".into()
        } else {
            "找不到 c9ext.json".into()
        });
    };
    if text.len() as u64 > MAX_MANIFEST_BYTES || size > MAX_MANIFEST_BYTES {
        return Err("manifest 超過 64 KiB".into());
    }
    // serde_json, like JSON.parse, is strict: no comments, no trailing comma.
    let raw: Value = serde_json::from_str(text).map_err(|_| "manifest 不是有效的 JSON")?;
    let Value::Object(raw) = raw else {
        return Err("manifest 不是 JSON 物件".into());
    };
    // R1: a JSON number 1, nothing else.
    if !is_one(raw.get("c9ext")) {
        return Err("manifest 版本不支援".into());
    }
    // R5, R10.
    let id = match raw.get("id").and_then(Value::as_str) {
        Some(id) if valid_id(id) => id,
        _ => return Err("id 格式不對（小寫字母開頭，只能有小寫字母、數字、-，最多 32 字）".into()),
    };
    if id != dir {
        return Err(format!("id「{id}」和目錄名「{dir}」不一樣"));
    }
    let Some(name) = text_field(raw.get("name"), MAX_NAME) else {
        return Err(format!("name 必須是 1–{MAX_NAME} 字的字串"));
    };
    // R11.
    let version = match raw.get("version") {
        Some(v) if is_semver(v) => v.as_str().unwrap_or_default(),
        _ => return Err("version 不是有效的 semver".into()),
    };
    let dev = match raw.get("dev") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("dev 必須是 true 或 false".into()),
    };

    // R6: only app and apiMinor; core (and anything else) is ignored.
    let min_version = match raw.get("minVersion") {
        None => None,
        Some(mv) => Some(parse_min_version(mv)?),
    };

    // R2, R3, R11.
    let pages = match raw.get("ui") {
        None => None,
        Some(ui) => Some(parse_ui(ui)?),
    };

    // R4: `core` is never read beyond the loose R12 check. Top-level
    // `contract` (R2) and every unknown field are ignored.
    let core = raw.get("core");
    Ok(Parsed {
        manifest: Manifest {
            id: id.to_owned(),
            name: name.to_owned(),
            version: version.to_owned(),
            min_version,
            pages,
            dev,
        },
        has_core: core.is_some(),
        warnings: core.map(core_warnings).unwrap_or_default(),
    })
}

fn parse_min_version(mv: &Value) -> Result<MinVersion, String> {
    let Value::Object(mv) = mv else {
        return Err("minVersion 必須是物件".into());
    };
    let mut out = MinVersion::default();
    if let Some(app) = mv.get("app") {
        if !is_semver(app) {
            return Err("minVersion.app 不是有效的 semver".into());
        }
        out.app = app.as_str().map(str::to_owned);
    }
    if let Some(n) = mv.get("apiMinor") {
        out.api_minor =
            Some(non_negative_integer(n).ok_or("minVersion.apiMinor 必須是 0 以上的整數")?);
    }
    Ok(out)
}

fn parse_ui(ui: &Value) -> Result<Vec<Page>, String> {
    let Value::Object(ui) = ui else {
        return Err("ui 必須是物件".into());
    };
    if ui.get("contract").and_then(Value::as_str) != Some(UI_CONTRACT) {
        return Err(format!("UI 合約不支援（需要 \"{UI_CONTRACT}\"）"));
    }
    let pages = match ui.get("pages") {
        Some(Value::Array(p)) if !p.is_empty() => p,
        _ => return Err("ui.pages 至少要有 1 頁".into()),
    };
    if pages.len() > MAX_PAGES {
        return Err(format!("ui.pages 最多 {MAX_PAGES} 頁"));
    }
    let mut out: Vec<Page> = Vec::with_capacity(pages.len());
    for (i, p) in pages.iter().enumerate() {
        let Value::Object(p) = p else {
            return Err(format!("ui.pages[{i}] 必須是物件"));
        };
        let id = match p.get("id").and_then(Value::as_str) {
            Some(id) if valid_id(id) => id,
            _ => return Err(format!("ui.pages[{i}].id 格式不對")),
        };
        if out.iter().any(|q| q.id == id) {
            return Err(format!("ui.pages 有兩個「{id}」"));
        }
        let Some(title) = text_field(p.get("title"), 40) else {
            return Err(format!("ui.pages[{i}].title 必須是 1–40 字的字串"));
        };
        let Some(entry) = is_entry_path(p.get("entry")) else {
            return Err(format!("ui.pages[{i}].entry 必須是目錄內的相對路徑"));
        };
        let icon = match p.get("icon") {
            None => None,
            Some(Value::String(s)) if js_len(s) <= 40 => Some(s.clone()),
            Some(_) => return Err(format!("ui.pages[{i}].icon 必須是字串")),
        };
        out.push(Page {
            id: id.to_owned(),
            title: title.to_owned(),
            entry: entry.to_owned(),
            icon,
        });
    }
    Ok(out)
}

// ── The files (mirror of src-tauri/src/ext.rs) ──────────────────────

/// `child` is strictly inside `parent` (both real paths).
fn inside(parent: &Path, child: &Path) -> bool {
    child != parent && child.starts_with(parent)
}

/// Why `<ext_dir>/<rel>` cannot be used, or None when it is a real file
/// inside `ext_dir`. `ext_dir` is a real path.
fn file_problem(ext_dir: &Path, rel: &str) -> Option<&'static str> {
    real_file(ext_dir, rel).err()
}

/// The real path of `<ext_dir>/<rel>` when it is a file inside `ext_dir`,
/// else why not.
fn real_file(ext_dir: &Path, rel: &str) -> Result<PathBuf, &'static str> {
    let mut p = ext_dir.to_path_buf();
    p.extend(rel.split('/'));
    // `symlink_metadata` tells a dangling symlink from a missing file.
    if fs::symlink_metadata(&p).is_err() {
        return Err("不存在");
    }
    let Ok(real) = fs::canonicalize(&p) else {
        return Err("是壞掉的 symlink");
    };
    if !inside(ext_dir, &real) {
        return Err("透過 symlink 指到 extension 目錄外");
    }
    if !real.is_file() {
        return Err("不是檔案");
    }
    Ok(real)
}

/// One path segment the `c9ext://` server sends: no dot file, `\`, `%`,
/// `:` or control character (`valid_segment` in the GUI crate).
fn servable_segment(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('.')
        && !s
            .chars()
            .any(|c| matches!(c, '\\' | '%' | ':' | '/') || c.is_control())
}

/// A page entry file name: `.js` or `.mjs` (case-insensitive), not a dot
/// file. The host loads an entry with `import()` (`Mount.svelte`), and the
/// `c9ext://` server sends a module only by these extensions
/// (`allowed_type` in the GUI crate).
fn module_name(name: &str) -> bool {
    !name.starts_with('.')
        && name
            .rsplit_once('.')
            .is_some_and(|(_, ext)| matches!(ext.to_ascii_lowercase().as_str(), "js" | "mjs"))
}

/// Reads the manifest as the GUI crate does: at most the limit, lossy UTF-8.
/// None (with the size) when it is missing, unreadable or too big.
fn read_manifest(ext_dir: &Path) -> (Option<String>, u64) {
    if file_problem(ext_dir, MANIFEST).is_some() {
        return (None, 0);
    }
    let Ok(f) = fs::File::open(ext_dir.join(MANIFEST)) else {
        return (None, 0);
    };
    let size = f.metadata().map_or(0, |m| m.len());
    if size > MAX_MANIFEST_BYTES {
        return (None, size);
    }
    let mut buf = Vec::new();
    if f.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut buf)
        .is_err()
    {
        return (None, size);
    }
    let size = size.max(buf.len() as u64);
    if size > MAX_MANIFEST_BYTES {
        return (None, size);
    }
    (Some(String::from_utf8_lossy(&buf).into_owned()), size)
}

// ── The check ───────────────────────────────────────────────────────

/// The result of `ext check`.
#[derive(Debug, Serialize)]
pub struct Report {
    /// The directory as given.
    pub path: String,
    /// The directory name the manifest `id` must equal.
    pub dir: String,
    pub ok: bool,
    /// Each one fails the check.
    pub errors: Vec<String>,
    /// Shown, but the check still passes.
    pub warnings: Vec<String>,
    /// Facts about the extension, such as "dev only".
    pub notes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<Manifest>,
}

impl Report {
    /// The text form: one status line, then one line per message.
    pub fn text(&self) -> String {
        let mut out = format!(
            "{} {} ({})\n",
            if self.ok { "PASS" } else { "FAIL" },
            self.dir,
            self.path
        );
        for (tag, list) in [
            ("error", &self.errors),
            ("warning", &self.warnings),
            ("note", &self.notes),
        ] {
            for m in list {
                out.push_str(&format!("  {tag}: {m}\n"));
            }
        }
        out
    }
}

/// The directory name: the last component as given (as the GUI lists it,
/// a symlinked directory keeps its own name), else the real path's.
fn dir_name(given: &Path, real: Option<&Path>) -> String {
    given
        .file_name()
        .or_else(|| real.and_then(Path::file_name))
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Checks one extension directory.
pub fn check(given: &Path) -> Report {
    let real = fs::canonicalize(given).ok().filter(|p| p.is_dir());
    let mut r = Report {
        path: given.display().to_string(),
        dir: dir_name(given, real.as_deref()),
        ok: false,
        errors: Vec::new(),
        warnings: Vec::new(),
        notes: Vec::new(),
        manifest: None,
    };
    let Some(ext_dir) = real else {
        r.errors.push("目錄不存在".into());
        return r;
    };

    let (text, size) = read_manifest(&ext_dir);
    match parse_manifest(text.as_deref(), &r.dir, size) {
        Ok(p) => {
            check_pages(&ext_dir, &p.manifest, &mut r.errors);
            r.warnings.extend(p.warnings);
            r.notes = notes(&p.manifest, p.has_core);
            r.manifest = Some(p.manifest);
        }
        Err(reason) => r.errors.push(reason),
    }

    for name in [RECIPE, PREVIEW] {
        if let Some(why) = file_problem(&ext_dir, name) {
            r.errors.push(format!("{name} {why}"));
        }
    }
    if file_problem(&ext_dir, PREVIEW).is_none() && !is_png(&ext_dir.join(PREVIEW)) {
        r.errors.push(format!("{PREVIEW} 不是 PNG 檔"));
    }
    r.ok = r.errors.is_empty();
    r
}

/// Each page entry is a module file the `c9ext://` server sends. Like the
/// server, this checks the requested path and the real file it resolves to,
/// so `index.js` → `module.txt` fails.
fn check_pages(ext_dir: &Path, m: &Manifest, errors: &mut Vec<String>) {
    for (i, page) in m.pages.iter().flatten().enumerate() {
        let entry = &page.entry;
        let real = match real_file(ext_dir, entry) {
            Ok(real) => real,
            Err(why) => {
                errors.push(format!("ui.pages[{i}].entry「{entry}」{why}"));
                continue;
            }
        };
        let name = entry.rsplit('/').next().unwrap_or(entry);
        let real_name = real.file_name().map(|n| n.to_string_lossy());
        if !entry.split('/').all(servable_segment)
            || !module_name(name)
            || !real_name.is_some_and(|n| module_name(&n))
        {
            errors.push(format!(
                "ui.pages[{i}].entry「{entry}」不是可載入的 module（檔名和 symlink 指到的檔名都要是 .js 或 .mjs，路徑不能有 . 開頭的段落）"
            ));
        }
    }
}

/// The first 8 bytes are the PNG signature.
fn is_png(path: &Path) -> bool {
    let mut sig = [0u8; 8];
    fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut sig))
        .is_ok()
        && &sig == PNG_SIGNATURE
}

/// What the host will do with a valid manifest (R3, R4, R6, R8).
fn notes(m: &Manifest, has_core: bool) -> Vec<String> {
    let mut out = Vec::new();
    if m.dev {
        out.push("dev: true：只在開發版顯示（dev_only）".into());
    }
    if m.pages.is_none() {
        out.push("沒有 ui：左欄不會有入口".into());
    }
    if has_core {
        out.push("含 core 部分：這一版不會執行它".into());
    }
    if let Some(mv) = &m.min_version {
        if let Some(app) = &mv.app {
            let this = env!("CARGO_PKG_VERSION");
            if let (Ok(want), Ok(have)) =
                (semver::Version::parse(app), semver::Version::parse(this))
            {
                if have < want {
                    out.push(format!(
                        "這個 c9watch 是 {this}，比 minVersion.app {app} 舊：會標 version_mismatch"
                    ));
                }
            }
        }
        #[cfg(all(unix, feature = "engine"))]
        if let Some(n) = mv.api_minor {
            let have = crate::engine_proto::wire::API_MINOR;
            if f64::from(have) < n {
                out.push(format!(
                    "這個 c9watch 的 c9p 是 1.{have}，比 minVersion.apiMinor {n} 舊：會標 version_mismatch"
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Map};
    use std::fs::{create_dir_all, write};

    /// A smallest valid PNG header is enough: the check reads 8 bytes.
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    fn parse(v: &Value, dir: &str) -> Result<Parsed, String> {
        parse_manifest(Some(&v.to_string()), dir, 0)
    }

    fn base() -> Value {
        json!({
            "c9ext": 1, "id": "hello", "name": "Hello", "version": "0.1.0",
            "ui": { "contract": "c9ui/1", "pages": [{ "id": "main", "title": "Hello", "entry": "index.js" }] }
        })
    }

    fn with(mut v: Value, f: impl FnOnce(&mut Map<String, Value>)) -> Value {
        f(v.as_object_mut().unwrap());
        v
    }

    fn reason(v: &Value) -> String {
        parse(v, "hello").unwrap_err()
    }

    /// Cases copied from `scripts/test-glass-ext.mjs` (the TS host's test).
    #[test]
    fn manifest_rules_match_the_host() {
        let p = parse(&base(), "hello").unwrap();
        assert_eq!(p.manifest.pages.as_ref().unwrap()[0].entry, "index.js");
        assert!(!p.manifest.dev && !p.has_core);

        // R1.
        for v in [json!("1"), json!(1.5), json!(2), Value::Null] {
            let m = with(base(), |o| {
                o.insert("c9ext".into(), v.clone());
            });
            assert_eq!(reason(&m), "manifest 版本不支援", "{v}");
        }
        let m = with(base(), |o| {
            o.remove("c9ext");
        });
        assert_eq!(reason(&m), "manifest 版本不支援");
        // `1.0 === 1` in JavaScript.
        let m = with(base(), |o| {
            o.insert("c9ext".into(), json!(1.0));
        });
        assert!(parse(&m, "hello").is_ok());

        // R5.
        for id in ["a", "hello", "office-map", "a1", &"a".repeat(32)] {
            let m = with(base(), |o| {
                o.insert("id".into(), json!(id));
            });
            assert!(parse(&m, id).is_ok(), "{id}");
        }
        for id in ["Hello", "1abc", "a.b", "a_b", "", "-a", &"a".repeat(33)] {
            let m = with(base(), |o| {
                o.insert("id".into(), json!(id));
            });
            assert!(
                parse(&m, id).unwrap_err().starts_with("id 格式不對"),
                "{id}"
            );
        }
        // R10.
        assert_eq!(
            parse(&base(), "other").unwrap_err(),
            "id「hello」和目錄名「other」不一樣"
        );

        // Strict JSON (R1).
        for text in ["{\"c9ext\":1,}", "// x\n{}", "[1]", "nope"] {
            let r = parse_manifest(Some(text), "hello", 0).unwrap_err();
            assert!(
                r == "manifest 不是有效的 JSON" || r == "manifest 不是 JSON 物件",
                "{text}"
            );
        }
        assert_eq!(
            parse_manifest(Some("[]"), "hello", 0).unwrap_err(),
            "manifest 不是 JSON 物件"
        );
        assert_eq!(
            parse_manifest(None, "hello", 0).unwrap_err(),
            "找不到 c9ext.json"
        );
        assert_eq!(
            parse_manifest(None, "hello", MAX_MANIFEST_BYTES + 1).unwrap_err(),
            "manifest 超過 64 KiB"
        );

        // R11 and names.
        for (k, v, want) in [
            ("name", json!(""), "name 必須是 1–64 字的字串"),
            ("name", json!(" \u{feff} "), "name 必須是 1–64 字的字串"),
            ("name", json!("x".repeat(65)), "name 必須是 1–64 字的字串"),
            ("version", json!("1.0"), "version 不是有效的 semver"),
            ("version", json!("01.0.0"), "version 不是有效的 semver"),
            ("version", json!("1.0.0-01"), "version 不是有效的 semver"),
            ("version", json!("١.٠.٠"), "version 不是有效的 semver"),
            ("dev", json!("yes"), "dev 必須是 true 或 false"),
            ("dev", Value::Null, "dev 必須是 true 或 false"),
            ("minVersion", json!("1.0.0"), "minVersion 必須是物件"),
            (
                "minVersion",
                json!({ "app": "x" }),
                "minVersion.app 不是有效的 semver",
            ),
            (
                "minVersion",
                json!({ "apiMinor": -1 }),
                "minVersion.apiMinor 必須是 0 以上的整數",
            ),
            (
                "minVersion",
                json!({ "apiMinor": 1.5 }),
                "minVersion.apiMinor 必須是 0 以上的整數",
            ),
            ("ui", json!([]), "ui 必須是物件"),
        ] {
            let m = with(base(), |o| {
                o.insert(k.into(), v.clone());
            });
            assert_eq!(reason(&m), want, "{k}={v}");
        }
        // 64 UTF-16 units of a 2-unit character is 32 characters.
        let m = with(base(), |o| {
            o.insert("name".into(), json!("😀".repeat(33)));
        });
        assert_eq!(reason(&m), "name 必須是 1–64 字的字串");
        for v in ["1.0.0-rc.1+build.5", "0.0.0", "10.20.30-alpha"] {
            let m = with(base(), |o| {
                o.insert("version".into(), json!(v));
            });
            assert!(parse(&m, "hello").is_ok(), "{v}");
        }
        let m = with(base(), |o| {
            o.insert(
                "minVersion".into(),
                json!({ "app": "0.12.0", "apiMinor": 2, "core": "x" }),
            );
        });
        let mv = parse(&m, "hello").unwrap().manifest.min_version.unwrap();
        assert_eq!(
            (mv.app.as_deref(), mv.api_minor),
            (Some("0.12.0"), Some(2.0))
        );

        // R2, R3, pages.
        let ui = |v: Value| {
            with(base(), |o| {
                o.insert("ui".into(), v);
            })
        };
        let page = |extra: Value| {
            let mut p = json!({ "id": "main", "title": "Hello", "entry": "index.js" });
            for (k, v) in extra.as_object().unwrap() {
                p[k] = v.clone();
            }
            ui(json!({ "contract": "c9ui/1", "pages": [p] }))
        };
        assert_eq!(
            reason(&ui(json!({ "pages": [] }))),
            "UI 合約不支援（需要 \"c9ui/1\"）"
        );
        assert_eq!(
            reason(&ui(json!({ "contract": "c9ui/2", "pages": [] }))),
            "UI 合約不支援（需要 \"c9ui/1\"）"
        );
        assert_eq!(
            reason(&ui(json!({ "contract": "c9ui/1", "pages": [] }))),
            "ui.pages 至少要有 1 頁"
        );
        let many: Vec<Value> = (0..17)
            .map(|i| json!({ "id": format!("p{i}"), "title": "x", "entry": "a.js" }))
            .collect();
        assert_eq!(
            reason(&ui(json!({ "contract": "c9ui/1", "pages": many }))),
            "ui.pages 最多 16 頁"
        );
        assert_eq!(
            reason(&ui(json!({ "contract": "c9ui/1", "pages": [1] }))),
            "ui.pages[0] 必須是物件"
        );
        assert_eq!(
            reason(&page(json!({ "id": "Main" }))),
            "ui.pages[0].id 格式不對"
        );
        assert_eq!(
            reason(&page(json!({ "title": " " }))),
            "ui.pages[0].title 必須是 1–40 字的字串"
        );
        assert_eq!(
            reason(&page(json!({ "icon": 3 }))),
            "ui.pages[0].icon 必須是字串"
        );
        let p = json!({ "id": "main", "title": "x", "entry": "a.js" });
        assert_eq!(
            reason(&ui(
                json!({ "contract": "c9ui/1", "pages": [p.clone(), p] })
            )),
            "ui.pages 有兩個「main」"
        );
        for entry in [
            "../x.js",
            "/abs.js",
            "a/../b.js",
            "./a.js",
            "a//b.js",
            "a\\b.js",
            "%2e%2e/x.js",
            "c:x.js",
            "a.js?x",
            "a.js#x",
            "",
            "a/",
        ] {
            assert_eq!(
                reason(&page(json!({ "entry": entry }))),
                "ui.pages[0].entry 必須是目錄內的相對路徑",
                "{entry}"
            );
        }
        assert!(parse(
            &page(json!({ "entry": "dist/index.js", "icon": "grid" })),
            "hello"
        )
        .is_ok());

        // R3: no ui is fine. R4, R12: core gives only warnings. Top-level
        // `contract` is ignored.
        let m = with(base(), |o| {
            o.remove("ui");
            o.insert("contract".into(), json!("whatever"));
            o.insert(
                "core".into(),
                json!({ "events": ["a", { "name": "b", "subscribers": ["local"] }, { "name": "" }, 3] }),
            );
        });
        let p = parse(&m, "hello").unwrap();
        assert!(p.manifest.pages.is_none() && p.has_core);
        assert_eq!(
            p.warnings,
            ["core.events[2] 格式不對", "core.events[3] 格式不對"]
        );
        let m = with(base(), |o| {
            o.insert("core".into(), json!({ "events": {} }));
        });
        assert_eq!(
            parse(&m, "hello").unwrap().warnings,
            ["core.events 不是陣列"]
        );
    }

    struct Fx {
        _tmp: tempfile::TempDir,
        root: PathBuf,
    }

    /// A temp root with one passing extension `<root>/<id>`.
    fn fx(id: &str) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        good(&root, id);
        Fx { _tmp: tmp, root }
    }

    fn good(root: &Path, id: &str) {
        let d = root.join(id);
        create_dir_all(&d).unwrap();
        let m = with(base(), |o| {
            o.insert("id".into(), json!(id));
        });
        write(d.join(MANIFEST), m.to_string()).unwrap();
        write(d.join("index.js"), "export function mount() {}").unwrap();
        write(d.join(RECIPE), "# x").unwrap();
        write(d.join(PREVIEW), PNG).unwrap();
    }

    fn set_manifest(dir: &Path, v: &Value) {
        write(dir.join(MANIFEST), v.to_string()).unwrap();
    }

    fn errors(dir: &Path) -> Vec<String> {
        let r = check(dir);
        assert_eq!(r.ok, r.errors.is_empty());
        r.errors
    }

    #[test]
    fn a_good_extension_passes() {
        let f = fx("hello");
        let r = check(&f.root.join("hello"));
        assert!(r.ok, "{:?}", r.errors);
        assert_eq!(r.dir, "hello");
        assert!(r.text().starts_with("PASS hello ("));
        // A trailing slash and `.` still name the directory.
        assert!(check(&f.root.join("hello/")).ok);
        assert_eq!(
            dir_name(Path::new("."), Some(&f.root.join("hello"))),
            "hello"
        );
    }

    #[test]
    fn the_repo_hello_extension_passes() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/hello");
        let r = check(&dir);
        assert!(r.ok, "{}", r.text());
        assert!(r.notes.iter().any(|n| n.contains("dev_only")));
    }

    #[test]
    fn broken_extensions_fail() {
        // Bad id.
        let f = fx("Bad_Id");
        assert!(errors(&f.root.join("Bad_Id"))[0].starts_with("id 格式不對"));

        // Missing `c9ext: 1`.
        let f = fx("hello");
        let d = f.root.join("hello");
        set_manifest(
            &d,
            &with(base(), |o| {
                o.remove("c9ext");
            }),
        );
        assert_eq!(errors(&d), ["manifest 版本不支援"]);

        // Id differs from the directory name.
        let f = fx("other");
        set_manifest(&f.root.join("other"), &base());
        assert_eq!(
            errors(&f.root.join("other")),
            ["id「hello」和目錄名「other」不一樣"]
        );

        // Missing entry.
        let f = fx("hello");
        let d = f.root.join("hello");
        fs::remove_file(d.join("index.js")).unwrap();
        assert_eq!(errors(&d), ["ui.pages[0].entry「index.js」不存在"]);

        // Entry escapes with `../`.
        let f = fx("hello");
        let d = f.root.join("hello");
        write(f.root.join("outside.js"), "x").unwrap();
        let m = with(base(), |o| {
            o["ui"]["pages"][0]["entry"] = json!("../outside.js");
        });
        set_manifest(&d, &m);
        assert_eq!(errors(&d), ["ui.pages[0].entry 必須是目錄內的相對路徑"]);

        // Missing RECIPE.md, missing preview.png, preview.png not a PNG.
        let f = fx("hello");
        let d = f.root.join("hello");
        fs::remove_file(d.join(RECIPE)).unwrap();
        assert_eq!(errors(&d), ["RECIPE.md 不存在"]);
        fs::remove_file(d.join(PREVIEW)).unwrap();
        assert_eq!(errors(&d), ["RECIPE.md 不存在", "preview.png 不存在"]);
        write(d.join(PREVIEW), "not a png").unwrap();
        assert_eq!(errors(&d), ["RECIPE.md 不存在", "preview.png 不是 PNG 檔"]);

        // No manifest at all; no directory at all.
        let f = fx("hello");
        let d = f.root.join("hello");
        fs::remove_file(d.join(MANIFEST)).unwrap();
        assert_eq!(errors(&d), ["找不到 c9ext.json"]);
        assert_eq!(errors(&f.root.join("nope")), ["目錄不存在"]);

        // A manifest over 64 KiB.
        let f = fx("hello");
        let d = f.root.join("hello");
        let big = with(base(), |o| {
            o.insert("pad".into(), json!("x".repeat(MAX_MANIFEST_BYTES as usize)));
        });
        set_manifest(&d, &big);
        assert_eq!(errors(&d), ["manifest 超過 64 KiB"]);
    }

    #[test]
    fn entries_the_server_will_not_send_fail() {
        let f = fx("hello");
        let d = f.root.join("hello");
        for (entry, ok) in [
            (".hidden.js", false),
            ("run.sh", false),
            ("page.html", false),
            ("dist/index.mjs", true),
            ("Upper.JS", true),
            ("style.css", false),
            ("data.json", false),
            ("notes.md", false),
        ] {
            if let Some((sub, _)) = entry.rsplit_once('/') {
                create_dir_all(d.join(sub)).unwrap();
            }
            write(d.join(entry), "x").unwrap();
            let m = with(base(), |o| {
                o["ui"]["pages"][0]["entry"] = json!(entry);
            });
            set_manifest(&d, &m);
            let r = check(&d);
            assert_eq!(r.ok, ok, "{entry}: {:?}", r.errors);
            if !ok {
                assert!(r.errors[0].contains("不是可載入的 module"), "{entry}");
            }
        }
        // A directory is not an entry.
        create_dir_all(d.join("dir.js")).unwrap();
        let m = with(base(), |o| {
            o["ui"]["pages"][0]["entry"] = json!("dir.js");
        });
        set_manifest(&d, &m);
        assert_eq!(errors(&d), ["ui.pages[0].entry「dir.js」不是檔案"]);
    }

    /// The server checks the real file's name too (GUI crate `respond`):
    /// a `.js` symlink to a file of another type, or to a dot file, fails.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_entry_is_checked_by_its_target_name() {
        use std::os::unix::fs::symlink;
        for (target, ok) in [
            ("module.txt", false),
            ("x.css", false),
            (".hidden.js", false),
            ("real.mjs", true),
        ] {
            let f = fx("hello");
            let d = f.root.join("hello");
            fs::remove_file(d.join("index.js")).unwrap();
            write(d.join(target), "x").unwrap();
            symlink(d.join(target), d.join("index.js")).unwrap();
            let r = check(&d);
            assert_eq!(r.ok, ok, "{target}: {:?}", r.errors);
            if !ok {
                assert_eq!(
                    r.errors,
                    ["ui.pages[0].entry「index.js」不是可載入的 module（檔名和 symlink 指到的檔名都要是 .js 或 .mjs，路徑不能有 . 開頭的段落）"]
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_leave_the_extension() {
        use std::os::unix::fs::symlink;
        let f = fx("hello");
        let d = f.root.join("hello");
        write(f.root.join("outside.js"), "x").unwrap();
        fs::remove_file(d.join("index.js")).unwrap();
        symlink(f.root.join("outside.js"), d.join("index.js")).unwrap();
        assert_eq!(
            errors(&d),
            ["ui.pages[0].entry「index.js」透過 symlink 指到 extension 目錄外"]
        );

        // A directory symlink out.
        fs::remove_file(d.join("index.js")).unwrap();
        create_dir_all(f.root.join("out")).unwrap();
        write(f.root.join("out/index.js"), "x").unwrap();
        symlink(f.root.join("out"), d.join("dist")).unwrap();
        set_manifest(
            &d,
            &with(base(), |o| {
                o["ui"]["pages"][0]["entry"] = json!("dist/index.js");
            }),
        );
        assert_eq!(
            errors(&d),
            ["ui.pages[0].entry「dist/index.js」透過 symlink 指到 extension 目錄外"]
        );

        // A dangling symlink.
        symlink(d.join("gone.js"), d.join("dangling.js")).unwrap();
        set_manifest(
            &d,
            &with(base(), |o| {
                o["ui"]["pages"][0]["entry"] = json!("dangling.js");
            }),
        );
        assert_eq!(
            errors(&d),
            ["ui.pages[0].entry「dangling.js」是壞掉的 symlink"]
        );

        // A symlink inside the extension is fine.
        write(d.join("real.js"), "x").unwrap();
        symlink(d.join("real.js"), d.join("alias.js")).unwrap();
        set_manifest(
            &d,
            &with(base(), |o| {
                o["ui"]["pages"][0]["entry"] = json!("alias.js");
            }),
        );
        assert!(check(&d).ok);

        // A manifest, RECIPE.md or preview.png symlinked out counts as missing.
        let g = fx("hello");
        let d = g.root.join("hello");
        for name in [MANIFEST, RECIPE, PREVIEW] {
            fs::rename(d.join(name), g.root.join(name)).unwrap();
            symlink(g.root.join(name), d.join(name)).unwrap();
        }
        assert_eq!(
            errors(&d),
            [
                "找不到 c9ext.json",
                "RECIPE.md 透過 symlink 指到 extension 目錄外",
                "preview.png 透過 symlink 指到 extension 目錄外",
            ]
        );
    }

    #[test]
    fn notes_say_what_the_host_will_do() {
        let f = fx("hello");
        let d = f.root.join("hello");
        set_manifest(
            &d,
            &with(base(), |o| {
                o.insert("dev".into(), json!(true));
                o.insert("minVersion".into(), json!({ "app": "999.0.0" }));
                o.insert("core".into(), json!({ "events": [1] }));
            }),
        );
        let r = check(&d);
        assert!(r.ok);
        assert_eq!(r.warnings, ["core.events[0] 格式不對"]);
        assert_eq!(r.notes.len(), 3, "{:?}", r.notes);
        assert!(r.notes[2].contains("999.0.0"));
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["ok"], true);
        assert_eq!(json["manifest"]["minVersion"]["app"], "999.0.0");
    }
}
