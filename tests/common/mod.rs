//! Shared test helpers.
//!
//! Fixture homes are stored with `dot-` instead of a leading `.` (for example `dot-claude`)
//! so that no tracked path contains `.claude/`. `fixture_home` copies a fixture into a
//! tempdir and renames the `dot-` prefix back to `.`. A directory named `fixture-tmp` becomes
//! `tmp` (a tracked `tmp/` path would be hidden by common ignore rules).

#![allow(dead_code)]

use std::fs;
use std::path::Path;

pub fn fixtures() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Copy `tests/fixtures/<rel>` into a new tempdir, renaming leading `dot-` to `.` and `fixture-tmp` to `tmp`.
/// Keep the returned guard alive for as long as the copy is used.
pub fn fixture_home(rel: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    copy_tree(&fixtures().join(rel), tmp.path());
    tmp
}

pub fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().to_string_lossy().into_owned();
        let name = match name.strip_prefix("dot-") {
            Some(rest) => format!(".{rest}"),
            None if name == "fixture-tmp" => "tmp".to_string(),
            None => name,
        };
        let dst = to.join(name);
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dst);
        } else {
            fs::copy(e.path(), &dst).unwrap();
        }
    }
}
