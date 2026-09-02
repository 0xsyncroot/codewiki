//! End-to-end integrity of the incremental path (`sync`) against a real
//! on-disk project: what a full index knows must survive an ordinary edit.

use std::fs;
use std::path::Path;
use std::process::Command;

fn count(db: &Path, sql: &str) -> i64 {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

/// Regression: Go's structural `implements` edges are synthesised from the
/// whole node inventory, not from unresolved refs. Re-storing a changed file
/// deletes its outgoing edges (correct — they are rebuilt from the fresh
/// parse), but the incremental resolution path never re-ran the synthesis,
/// so an ordinary edit of a Go file permanently lost `Circle -> Shape`.
///
/// The fixture has more than ten files on purpose: with fewer, one changed
/// file exceeds the 10 % threshold and sync falls back to the FULL resolution
/// path, which does re-synthesise — masking the bug.
#[test]
#[serial_test::serial]
fn go_structural_implements_survive_incremental_edit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("git not available; skipping");
        return;
    }
    Command::new("git")
        .args(["init", "-q", "."])
        .current_dir(&root)
        .status()
        .unwrap();

    write(
        &root,
        "shape.go",
        "package shapes\n\ntype Shape interface {\n\tArea() float64\n}\n",
    );
    write(
        &root,
        "circle.go",
        "package shapes\n\ntype Circle struct{ R float64 }\n\nfunc (c Circle) Area() float64 { return 3.14 * c.R * c.R }\n",
    );
    for i in 1..=20 {
        write(
            &root,
            &format!("filler{i}.go"),
            &format!("package shapes\n\nfunc Filler{i}() int {{ return {i} }}\n"),
        );
    }

    codewiki_cli::commands::init::run(Some(root.clone()), false).unwrap();
    let db = root.join(".codewiki").join("codewiki.db");
    let implements = "SELECT count(*) FROM edges WHERE kind = 'implements'";
    assert_eq!(
        count(&db, implements),
        1,
        "full index must synthesise Circle -> Shape"
    );

    // An edit that changes nothing structural.
    let circle = root.join("circle.go");
    let mut body = fs::read_to_string(&circle).unwrap();
    body.push_str("// touch\n");
    fs::write(&circle, body).unwrap();

    codewiki_cli::commands::sync::run(Some(root.clone())).unwrap();
    assert_eq!(
        count(&db, implements),
        1,
        "the incremental path must re-synthesise structural implements edges"
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM nodes WHERE name IN ('Circle','Shape')"
        ),
        2
    );
}

/// Regression: `codewiki index` over an existing database never pruned files
/// that vanished from disk — only `sync` computed removals. A deleted file
/// kept its nodes and edges as ghosts, and a renamed file existed twice.
#[test]
#[serial_test::serial]
fn full_index_prunes_files_that_vanished_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    if Command::new("git").arg("--version").output().is_err() {
        return;
    }
    Command::new("git")
        .args(["init", "-q", "."])
        .current_dir(&root)
        .status()
        .unwrap();

    write(
        &root,
        "keep.ts",
        "export function keep(): number { return 1 }\n",
    );
    write(
        &root,
        "gone.ts",
        "export function gone(): number { return 2 }\n",
    );
    write(
        &root,
        "old_name.ts",
        "export function renamed(): number { return 3 }\n",
    );
    codewiki_cli::commands::init::run(Some(root.clone()), false).unwrap();
    let db = root.join(".codewiki").join("codewiki.db");
    assert_eq!(
        count(&db, "SELECT count(*) FROM nodes WHERE name = 'gone'"),
        1
    );

    fs::remove_file(root.join("gone.ts")).unwrap();
    fs::rename(root.join("old_name.ts"), root.join("new_name.ts")).unwrap();

    codewiki_cli::commands::index::run(Some(root.clone())).unwrap();
    assert_eq!(
        count(&db, "SELECT count(*) FROM nodes WHERE name = 'gone'"),
        0,
        "a deleted file's nodes must not survive a full re-index"
    );
    assert_eq!(
        count(&db, "SELECT count(*) FROM nodes WHERE name = 'renamed'"),
        1,
        "a renamed file must exist once, under its new path"
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM files WHERE path LIKE '%old_name.ts'"
        ),
        0
    );
    assert_eq!(
        count(&db, "SELECT count(*) FROM nodes WHERE name = 'keep'"),
        1
    );
}

/// Regression: `init --path .` stored `./relative` paths while `sync` from the
/// cwd stored absolute ones, so the first sync classified every file as
/// removed + added (2,117 files on a real repo) and `root_path` stayed `.`.
/// The root is now canonicalised once, for every command.
#[test]
#[serial_test::serial]
fn relative_root_and_cwd_root_share_one_path_spelling() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    if Command::new("git").arg("--version").output().is_err() {
        return;
    }
    Command::new("git")
        .args(["init", "-q", "."])
        .current_dir(&root)
        .status()
        .unwrap();
    for i in 0..5 {
        write(
            &root,
            &format!("f{i}.ts"),
            &format!("export function f{i}(): number {{ return {i} }}\n"),
        );
    }

    let prev = std::env::current_dir().unwrap();
    std::env::set_current_dir(&root).unwrap();
    let res = (|| {
        codewiki_cli::commands::init::run(Some(std::path::PathBuf::from(".")), false)?;
        let db = root.join(".codewiki").join("codewiki.db");
        assert_eq!(
            count(&db, "SELECT count(*) FROM files WHERE path LIKE './%'"),
            0,
            "stored paths must not carry the relative spelling"
        );
        let ids_before: i64 = count(&db, "SELECT sum(length(id)) FROM nodes");
        codewiki_cli::commands::sync::run(None)?;
        assert_eq!(
            count(&db, "SELECT count(*) FROM files WHERE path LIKE './%'"),
            0
        );
        assert_eq!(
            count(&db, "SELECT sum(length(id)) FROM nodes"),
            ids_before,
            "a no-op sync must not rewrite the index under a different path spelling"
        );
        Ok::<(), anyhow::Error>(())
    })();
    std::env::set_current_dir(prev).unwrap();
    res.unwrap();
}
