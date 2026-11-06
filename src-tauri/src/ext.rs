//! `c9ext://` file serving and the `ext_list` command (glass build plan
//! §3.2, G7). The GUI crate serves extension files; the core never does
//! (D32: the core does not depend on tauri).
//!
//!   ext_list                        → { extensions: ExtCandidate[] }
//!   c9ext://localhost/<id>/<path>   → a file from the first root that has <id>
//!
//! Roots, in order (R9: the first-party root wins a shared id):
//!   first: the app bundle's `extensions/` (in a debug build: the repo's)
//!   third: `<engine home>/extensions/` (D31 Q13: add a directory, restart)
//!
//! The host validates the manifests (`src/shells/glass/ext/manifest.ts`);
//! this file only reads files. The rules mirror the Vite dev middleware
//! (`scripts/vite-c9ext-dev.mjs`). This is not isolation (D31: extensions
//! are trusted code); it only keeps a path inside its extension directory.
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::http::{header, Method, Request, Response, StatusCode};

/// The scheme name. The CSP in `tauri.conf.json` allows `c9ext:`.
pub const SCHEME: &str = "c9ext";
/// Same limit as the host's `MAX_MANIFEST_BYTES`.
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MANIFEST: &str = "c9ext.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    First,
    Third,
}

/// One extensions root. `dir` is a real (canonical) path.
#[derive(Clone, Debug)]
pub struct Root {
    pub source: Source,
    pub dir: PathBuf,
}

/// The roots this process serves from, found once at startup (Q13).
#[derive(Clone, Debug, Default)]
pub struct ExtRoots(pub Vec<Root>);

/// One directory in a root, before validation (the host's `ExtCandidate`).
/// `base` is left out: the host defaults it to `c9ext://localhost/`.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Candidate {
    pub dir: String,
    pub source: Source,
    /// The raw `c9ext.json` text; null when missing, unreadable or too big.
    pub manifest: Option<String>,
    pub size: u64,
}

#[derive(Debug, Serialize)]
pub struct ExtList {
    pub extensions: Vec<Candidate>,
}

impl ExtRoots {
    /// Canonicalizes each root. A root that does not exist is skipped.
    pub fn new(roots: impl IntoIterator<Item = (Source, PathBuf)>) -> Self {
        Self(
            roots
                .into_iter()
                .filter_map(|(source, dir)| {
                    let dir = fs::canonicalize(dir).ok()?;
                    dir.is_dir().then_some(Root { source, dir })
                })
                .collect(),
        )
    }

    /// The app's two roots.
    pub fn discover<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Self {
        use tauri::Manager;
        let first = if cfg!(debug_assertions) {
            // `tauri dev`: the repo's extensions, so edits show on restart.
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../extensions")
        } else {
            app.path()
                .resource_dir()
                .map(|d| d.join("extensions"))
                .unwrap_or_default()
        };
        let mut roots = vec![(Source::First, first)];
        #[cfg(unix)]
        if let Ok(home) = crate::engine_proto::home::engine_home() {
            roots.push((Source::Third, home.join("extensions")));
        }
        Self::new(roots)
    }

    /// Every extension directory in every root, with its raw manifest.
    pub fn list(&self) -> ExtList {
        let mut out = Vec::new();
        for root in &self.0 {
            let Ok(entries) = fs::read_dir(&root.dir) else {
                continue;
            };
            let mut names: Vec<String> = entries
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() || t.is_symlink()))
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| !n.starts_with('.'))
                .collect();
            names.sort();
            for dir in names {
                let Some(ext_dir) = real_ext_dir(root, &dir) else {
                    continue;
                };
                let (manifest, size) = match real_file_in(&ext_dir, &[MANIFEST]) {
                    Some(file) => read_manifest(&file),
                    None => (None, 0),
                };
                out.push(Candidate {
                    dir,
                    source: root.source,
                    manifest,
                    size,
                });
            }
        }
        ExtList { extensions: out }
    }

    /// The real path of `<root>/<id>/<rel>` in the first root that has
    /// `<id>`, or None. `raw_path` is the URL path, still percent-encoded.
    pub fn resolve(&self, raw_path: &str) -> Option<PathBuf> {
        let path = raw_path.strip_prefix('/')?;
        let (id, rel) = path.split_once('/')?;
        if !valid_id(id) {
            return None;
        }
        // Decode each segment on its own, so `%2F` can never add a segment.
        let segs: Vec<String> = rel.split('/').map(percent_decode).collect::<Option<_>>()?;
        if segs.is_empty() || !segs.iter().all(|s| valid_segment(s)) {
            return None;
        }
        allowed_type(segs.last()?)?;
        let segs: Vec<&str> = segs.iter().map(String::as_str).collect();
        for root in &self.0 {
            // R9: the first root that has the id owns it, file or not.
            if let Some(ext_dir) = real_ext_dir(root, id) {
                return real_file_in(&ext_dir, &segs);
            }
        }
        None
    }

    /// The response for one `c9ext://` request.
    pub fn respond(&self, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
        let method = request.method();
        if method != Method::GET && method != Method::HEAD {
            return status(StatusCode::METHOD_NOT_ALLOWED);
        }
        // macOS and Linux webviews send `c9ext://localhost/...`.
        if request.uri().host() != Some("localhost") {
            return status(StatusCode::NOT_FOUND);
        }
        let Some(file) = self.resolve(request.uri().path()) else {
            return status(StatusCode::NOT_FOUND);
        };
        let Some(mime) = file
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(allowed_type)
        else {
            return status(StatusCode::NOT_FOUND);
        };
        let body = if method == Method::HEAD {
            Vec::new()
        } else {
            match fs::read(&file) {
                Ok(b) => b,
                Err(_) => return status(StatusCode::INTERNAL_SERVER_ERROR),
            }
        };
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime)
            .header("X-Content-Type-Options", "nosniff")
            .header(header::CACHE_CONTROL, "no-store")
            // The page (tauri://localhost, or the Vite dev server) imports
            // modules from another origin, so module loads need CORS.
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .body(body)
            .unwrap_or_else(|_| status(StatusCode::INTERNAL_SERVER_ERROR))
    }
}

fn status(code: StatusCode) -> Response<Vec<u8>> {
    let mut r = Response::new(Vec::new());
    *r.status_mut() = code;
    r
}

/// The manifest id rule (D41): the directory name must be a valid id.
pub fn valid_id(id: &str) -> bool {
    let b = id.as_bytes();
    (1..=32).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// One decoded path segment: no empty, `.`/`..`, dot file, `\`, `%`, `:`,
/// or control character. An absolute path or `//` gives an empty segment.
fn valid_segment(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('.')
        && !s
            .chars()
            .any(|c| matches!(c, '\\' | '%' | ':' | '/') || c.is_control())
}

/// The MIME type of an allowed file name, by extension (case-insensitive).
pub fn allowed_type(name: &str) -> Option<&'static str> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "woff2" => "font/woff2",
        "md" => "text/markdown; charset=utf-8",
        _ => return None,
    })
}

/// Percent-decodes one segment. None on a bad escape or invalid UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = b.get(i + 1..i + 3)?;
            if !hex.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            out.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `child` is strictly inside `parent` (both real paths).
fn inside(parent: &Path, child: &Path) -> bool {
    child != parent && child.starts_with(parent)
}

/// The real path of `<root>/<name>`, or None when it is missing, not a
/// directory, or (through a symlink) not inside the root. Such a directory
/// counts as absent: it neither lists nor shadows.
fn real_ext_dir(root: &Root, name: &str) -> Option<PathBuf> {
    let dir = fs::canonicalize(root.dir.join(name)).ok()?;
    (inside(&root.dir, &dir) && dir.is_dir()).then_some(dir)
}

/// The real path of `<ext_dir>/<segs...>` when it is a file inside `ext_dir`.
fn real_file_in(ext_dir: &Path, segs: &[&str]) -> Option<PathBuf> {
    let mut p = ext_dir.to_path_buf();
    p.extend(segs);
    let real = fs::canonicalize(p).ok()?;
    (inside(ext_dir, &real) && real.is_file()).then_some(real)
}

/// Reads at most the limit, so a file that grows after `stat` cannot be big.
fn read_manifest(file: &Path) -> (Option<String>, u64) {
    let Ok(f) = fs::File::open(file) else {
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
    if buf.len() as u64 > MAX_MANIFEST_BYTES {
        return (None, size);
    }
    (Some(String::from_utf8_lossy(&buf).into_owned()), size)
}

/// `localCommand('ext_list')`. The host reads it once per window.
#[tauri::command]
pub async fn ext_list(roots: tauri::State<'_, ExtRoots>) -> Result<ExtList, String> {
    let roots = roots.inner().clone();
    tauri::async_runtime::spawn_blocking(move || roots.list())
        .await
        .map_err(|e| e.to_string())
}

/// Registers the `c9ext` scheme. Reads happen off the main thread.
pub fn register<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    use tauri::Manager;
    builder.register_asynchronous_uri_scheme_protocol(SCHEME, |ctx, request, responder| {
        let roots = ctx
            .app_handle()
            .try_state::<ExtRoots>()
            .map(|s| s.inner().clone())
            .unwrap_or_default();
        tauri::async_runtime::spawn_blocking(move || responder.respond(roots.respond(&request)));
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{create_dir_all, write};

    fn ext(root: &Path, id: &str, manifest: &str) {
        create_dir_all(root.join(id)).unwrap();
        write(root.join(id).join(MANIFEST), manifest).unwrap();
        write(root.join(id).join("index.js"), format!("// {id}")).unwrap();
    }

    struct Fx {
        _tmp: tempfile::TempDir,
        first: PathBuf,
        third: PathBuf,
        outside: PathBuf,
        roots: ExtRoots,
    }

    fn fx() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("bundle/extensions");
        let third = tmp.path().join("home/extensions");
        let outside = tmp.path().join("outside");
        create_dir_all(&first).unwrap();
        create_dir_all(&third).unwrap();
        create_dir_all(&outside).unwrap();
        write(outside.join("secret.js"), "secret").unwrap();
        ext(&first, "hello", r#"{"id":"hello","src":"first"}"#);
        ext(&third, "hello", r#"{"id":"hello","src":"third"}"#);
        ext(&third, "office", r#"{"id":"office"}"#);
        let roots = ExtRoots::new([
            (Source::First, first.clone()),
            (Source::Third, third.clone()),
        ]);
        Fx {
            _tmp: tmp,
            first,
            third,
            outside,
            roots,
        }
    }

    fn get(roots: &ExtRoots, url: &str) -> Response<Vec<u8>> {
        roots.respond(&Request::builder().uri(url).body(Vec::new()).unwrap())
    }

    #[test]
    fn serves_a_file_with_its_mime_type() {
        let f = fx();
        let r = get(&f.roots, "c9ext://localhost/office/index.js");
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            r.headers()[header::CONTENT_TYPE],
            "text/javascript; charset=utf-8"
        );
        assert_eq!(r.headers()["X-Content-Type-Options"], "nosniff");
        assert_eq!(r.body(), b"// office");
        create_dir_all(f.third.join("office/img")).unwrap();
        write(f.third.join("office/img/a.SVG"), "<svg/>").unwrap();
        let r = get(&f.roots, "c9ext://localhost/office/img/a.SVG");
        assert_eq!(r.headers()[header::CONTENT_TYPE], "image/svg+xml");
        assert_eq!(
            get(&f.roots, "c9ext://localhost/office/c9ext.json").headers()[header::CONTENT_TYPE],
            "application/json"
        );
    }

    #[test]
    fn same_id_first_party_shadows_third_party() {
        let f = fx();
        assert_eq!(
            get(&f.roots, "c9ext://localhost/hello/index.js").body(),
            b"// hello"
        );
        let p = f.roots.resolve("/hello/index.js").unwrap();
        assert!(p.starts_with(fs::canonicalize(&f.first).unwrap()));
        // The first root owns the id even for a file only the third has.
        write(f.third.join("hello/extra.js"), "x").unwrap();
        assert_eq!(
            get(&f.roots, "c9ext://localhost/hello/extra.js").status(),
            StatusCode::NOT_FOUND
        );
        // Both copies are listed; the host marks the third one shadowed.
        let list = f.roots.list().extensions;
        let hello: Vec<_> = list
            .iter()
            .filter(|c| c.dir == "hello")
            .map(|c| c.source)
            .collect();
        assert_eq!(hello, [Source::First, Source::Third]);
    }

    #[test]
    fn rejects_path_traversal() {
        let f = fx();
        for url in [
            "c9ext://localhost/hello/../office/index.js",
            "c9ext://localhost/hello/./index.js",
            "c9ext://localhost/../outside/secret.js",
            "c9ext://localhost/hello//index.js",
            "c9ext://localhost/hello/.hidden.js",
            "c9ext://localhost/hello/C:/x.js",
            "c9ext://localhost/hello/",
            "c9ext://localhost/hello",
        ] {
            assert_eq!(get(&f.roots, url).status(), StatusCode::NOT_FOUND, "{url}");
        }
        assert_eq!(f.roots.resolve("/hello//etc/passwd.js"), None);
        assert_eq!(f.roots.resolve("/hello/a\\b.js"), None);
        assert_eq!(f.roots.resolve("/hello/sub\\..\\..\\x.js"), None);
        assert_eq!(f.roots.resolve("hello/index.js"), None);
    }

    #[test]
    fn rejects_percent_encoded_traversal() {
        let f = fx();
        for path in [
            "/hello/%2e%2e/%2e%2e/outside/secret.js",
            "/hello/..%2f..%2foutside%2fsecret.js",
            "/hello/%2E%2E%2Fsecret.js",
            "/hello/..%5c..%5csecret.js",
            "/hello/%252e%252e/secret.js",
            "/hello/%2findex.js",
            "/hello/index.js%00.png",
            "/hello/%zz.js",
            "/hello/%+1.js",
            "/%2e%2e/outside/secret.js",
            "/hell%6f/index.js",
        ] {
            assert_eq!(f.roots.resolve(path), None, "{path}");
        }
        // An ordinary escape still decodes.
        write(f.third.join("office/a b.js"), "x").unwrap();
        assert!(f.roots.resolve("/office/a%20b.js").is_some());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_leave_the_extension() {
        let f = fx();
        // A file symlink to outside.
        std::os::unix::fs::symlink(f.outside.join("secret.js"), f.third.join("office/link.js"))
            .unwrap();
        assert_eq!(
            get(&f.roots, "c9ext://localhost/office/link.js").status(),
            StatusCode::NOT_FOUND
        );
        // A directory symlink to outside.
        std::os::unix::fs::symlink(&f.outside, f.third.join("office/out")).unwrap();
        assert_eq!(f.roots.resolve("/office/out/secret.js"), None);
        // A symlink to another extension's file is outside its own dir.
        std::os::unix::fs::symlink(
            f.third.join("hello/index.js"),
            f.third.join("office/peer.js"),
        )
        .unwrap();
        assert_eq!(f.roots.resolve("/office/peer.js"), None);
        // An extension dir that is a symlink out of the root: absent.
        write(f.outside.join(MANIFEST), "{}").unwrap();
        std::os::unix::fs::symlink(&f.outside, f.third.join("evil")).unwrap();
        assert_eq!(f.roots.resolve("/evil/secret.js"), None);
        assert!(f.roots.list().extensions.iter().all(|c| c.dir != "evil"));
        // A symlink inside the extension dir is fine.
        std::os::unix::fs::symlink(
            f.third.join("office/index.js"),
            f.third.join("office/alias.js"),
        )
        .unwrap();
        assert!(f.roots.resolve("/office/alias.js").is_some());
    }

    #[test]
    fn rejects_disallowed_extensions_and_bad_ids() {
        let f = fx();
        for name in ["run.sh", "page.html", "x.wasm", "noext", "a.js.exe"] {
            write(f.third.join("office").join(name), "x").unwrap();
            assert_eq!(
                get(&f.roots, &format!("c9ext://localhost/office/{name}")).status(),
                StatusCode::NOT_FOUND,
                "{name}"
            );
        }
        for name in ["a.mjs", "a.css", "a.png", "a.woff2", "a.md"] {
            write(f.third.join("office").join(name), "x").unwrap();
            assert_eq!(
                get(&f.roots, &format!("c9ext://localhost/office/{name}")).status(),
                StatusCode::OK,
                "{name}"
            );
        }
        for id in ["Hello", "1abc", "a.b", "a_b", "", &"a".repeat(33)] {
            assert!(!valid_id(id), "{id}");
        }
        assert!(valid_id("a") && valid_id("my-ext-2") && valid_id(&"a".repeat(32)));
        create_dir_all(f.third.join("Bad.Id")).unwrap();
        write(f.third.join("Bad.Id/index.js"), "x").unwrap();
        assert_eq!(f.roots.resolve("/Bad.Id/index.js"), None);
    }

    #[test]
    fn only_get_and_head_on_localhost() {
        let f = fx();
        let req = |m: &str, url: &str| {
            f.roots.respond(
                &Request::builder()
                    .method(m)
                    .uri(url)
                    .body(Vec::new())
                    .unwrap(),
            )
        };
        assert_eq!(
            req("POST", "c9ext://localhost/office/index.js").status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
        let head = req("HEAD", "c9ext://localhost/office/index.js");
        assert_eq!(head.status(), StatusCode::OK);
        assert!(head.body().is_empty());
        assert_eq!(
            req("GET", "c9ext://evil.com/office/index.js").status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get(&f.roots, "c9ext://localhost/missing/index.js").status(),
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn list_sends_raw_manifests_and_withholds_oversize_ones() {
        let f = fx();
        let big = format!(
            "{{\"pad\":\"{}\"}}",
            "x".repeat(MAX_MANIFEST_BYTES as usize)
        );
        ext(&f.third, "big", &big);
        create_dir_all(f.third.join("empty")).unwrap();
        create_dir_all(f.third.join(".git")).unwrap();
        write(f.third.join("loose.js"), "x").unwrap();
        let exact = "x".repeat(MAX_MANIFEST_BYTES as usize);
        ext(&f.third, "exact", &exact);
        let list = f.roots.list().extensions;
        let names: Vec<_> = list.iter().map(|c| (c.dir.as_str(), c.source)).collect();
        assert_eq!(
            names,
            [
                ("hello", Source::First),
                ("big", Source::Third),
                ("empty", Source::Third),
                ("exact", Source::Third),
                ("hello", Source::Third),
                ("office", Source::Third),
            ]
        );
        let by = |d: &str| {
            list.iter()
                .find(|c| c.dir == d && c.source == Source::Third)
                .unwrap()
        };
        assert_eq!(by("big").manifest, None);
        assert_eq!(by("big").size, big.len() as u64);
        assert_eq!(by("empty").manifest, None);
        assert_eq!(by("exact").manifest.as_deref(), Some(exact.as_str()));
        assert_eq!(by("office").manifest.as_deref(), Some(r#"{"id":"office"}"#));
        // The JSON shape the TS host reads.
        let json = serde_json::to_value(f.roots.list()).unwrap();
        assert_eq!(json["extensions"][0]["source"], "first");
        assert_eq!(json["extensions"][0]["dir"], "hello");
        assert!(json["extensions"][1]["manifest"].is_null());
        assert!(json["extensions"][0].get("base").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn manifest_symlinked_out_is_not_read() {
        let f = fx();
        create_dir_all(f.third.join("sneaky")).unwrap();
        write(f.outside.join("m.json"), "{\"id\":\"sneaky\"}").unwrap();
        std::os::unix::fs::symlink(
            f.outside.join("m.json"),
            f.third.join("sneaky").join(MANIFEST),
        )
        .unwrap();
        let list = f.roots.list().extensions;
        let c = list.iter().find(|c| c.dir == "sneaky").unwrap();
        assert_eq!(c.manifest, None);
    }

    #[test]
    fn missing_roots_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let roots = ExtRoots::new([(Source::First, tmp.path().join("nope"))]);
        assert!(roots.0.is_empty());
        assert!(roots.list().extensions.is_empty());
        assert_eq!(roots.resolve("/hello/index.js"), None);
    }
}
