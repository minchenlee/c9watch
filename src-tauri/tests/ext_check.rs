//! `c9watch ext check` through the real binary (G11): exit codes and
//! output. CLI-only builds: in a GUI build a routing bug would open the app,
//! so the routing itself is a unit test (`cli::dispatch_tests`).
#![cfg(all(unix, not(feature = "gui")))]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_c9watch");
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().unwrap()
}

fn check(dir: &Path) -> Output {
    run(&["ext", "check", dir.to_str().unwrap()])
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// `<root>/<name>` with a passing extension whose manifest id is `id`.
fn good(root: &Path, name: &str, id: &str) -> PathBuf {
    let d = root.join(name);
    std::fs::create_dir_all(&d).unwrap();
    let m = format!(
        r#"{{"c9ext":1,"id":"{id}","name":"X","version":"1.0.0","ui":{{"contract":"c9ui/1","pages":[{{"id":"main","title":"X","entry":"index.js"}}]}}}}"#
    );
    std::fs::write(d.join("c9ext.json"), m).unwrap();
    std::fs::write(d.join("index.js"), "export function mount() {}").unwrap();
    std::fs::write(d.join("RECIPE.md"), "# X").unwrap();
    std::fs::write(d.join("preview.png"), PNG).unwrap();
    d
}

#[test]
fn hello_passes() {
    let hello = Path::new(env!("CARGO_MANIFEST_DIR")).join("../extensions/hello");
    let o = check(&hello);
    assert_eq!(o.status.code(), Some(0), "{}", stdout(&o));
    assert!(stdout(&o).starts_with("PASS hello ("));

    let o = run(&["ext", "check", hello.to_str().unwrap(), "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["manifest"]["id"], "hello");
}

#[test]
fn broken_fixtures_fail() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::write(root.join("outside.js"), "x").unwrap();

    let mut cases: Vec<(PathBuf, &str)> = Vec::new();

    let d = good(&root, "Bad_Id", "Bad_Id");
    cases.push((d, "id 格式不對"));

    let d = good(&root, "no-format", "no-format");
    let m = std::fs::read_to_string(d.join("c9ext.json")).unwrap();
    std::fs::write(d.join("c9ext.json"), m.replace(r#""c9ext":1,"#, "")).unwrap();
    cases.push((d, "manifest 版本不支援"));

    let d = good(&root, "no-entry", "no-entry");
    std::fs::remove_file(d.join("index.js")).unwrap();
    cases.push((d, "「index.js」不存在"));

    let d = good(&root, "dotdot", "dotdot");
    let m = std::fs::read_to_string(d.join("c9ext.json")).unwrap();
    std::fs::write(d.join("c9ext.json"), m.replace("index.js", "../outside.js")).unwrap();
    cases.push((d, "entry 必須是目錄內的相對路徑"));

    let d = good(&root, "symlink", "symlink");
    std::fs::remove_file(d.join("index.js")).unwrap();
    std::os::unix::fs::symlink(root.join("outside.js"), d.join("index.js")).unwrap();
    cases.push((d, "透過 symlink 指到 extension 目錄外"));

    let d = good(&root, "no-recipe", "no-recipe");
    std::fs::remove_file(d.join("RECIPE.md")).unwrap();
    cases.push((d, "RECIPE.md 不存在"));

    let d = good(&root, "no-preview", "no-preview");
    std::fs::remove_file(d.join("preview.png")).unwrap();
    cases.push((d, "preview.png 不存在"));

    let d = good(&root, "wrong-dir", "other");
    cases.push((d, "和目錄名「wrong-dir」不一樣"));

    for (dir, want) in &cases {
        let o = check(dir);
        let out = stdout(&o);
        assert_eq!(o.status.code(), Some(1), "{}: {out}", dir.display());
        assert!(out.starts_with("FAIL "), "{out}");
        assert!(
            out.contains(want),
            "{}: want {want}, got {out}",
            dir.display()
        );
    }
}

#[test]
fn help_prints_usage() {
    let o = run(&["ext", "check", "--help"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(stdout(&o).contains("Usage: c9watch ext check"));
}
