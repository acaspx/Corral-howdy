//! The filesystem sandbox (M2 item 6, TP-FS-01..10, security model T8): an agent's file tools reach
//! only its own folders. Every path is opened beneath the folder's handle, so `..`, absolute paths
//! and links can't lead out, even if the folder changes after the sandbox was made. Names that APFS
//! would treat as the same file (case, Unicode normalization) can't silently overwrite another.
//! Links inside the folder work when their target is relative; an absolute target is refused.

use corral_core::domain::ActionClass;
use corral_tools::fs::{
    FsDelete, FsError, FsRead, FsWrite, MAX_READ_BYTES, MAX_WRITE_BYTES, Sandbox,
};
use corral_tools::{Tool, ToolError};
use serde_json::json;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A fresh folder with `root/` (the agent's) and `outside/` (not the agent's), cleaned up on drop.
struct Folders {
    base: PathBuf,
}

impl Folders {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "corral-fs-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        if base.exists() {
            std::fs::remove_dir_all(&base).expect("clear");
        }
        std::fs::create_dir_all(base.join("root/docs")).expect("root");
        std::fs::create_dir_all(base.join("outside")).expect("outside");
        std::fs::write(base.join("root/notes.md"), "my notes").expect("notes");
        std::fs::write(base.join("outside/secret.txt"), "the secret").expect("secret");
        Self { base }
    }

    fn root(&self) -> PathBuf {
        self.base.join("root")
    }

    fn outside(&self) -> PathBuf {
        self.base.join("outside")
    }

    fn sandbox(&self) -> Sandbox {
        Sandbox::new([self.root()]).expect("sandbox")
    }

    fn entries(&self, dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("list")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for Folders {
    fn drop(&mut self) {
        // Best effort: a leftover temp folder is harmless, and a panic here would hide the test's.
        let cleaned = std::fs::remove_dir_all(&self.base);
        drop(cleaned);
    }
}

fn secret_is_untouched(f: &Folders) {
    assert_eq!(
        std::fs::read_to_string(f.outside().join("secret.txt")).expect("still there"),
        "the secret"
    );
}

// ---- TP-FS-01..02: the path itself ------------------------------------------------------------

#[test]
fn tp_fs_01_parent_components_are_refused_wherever_they_are() {
    let f = Folders::new();
    let sb = f.sandbox();
    for path in [
        "../outside/secret.txt",
        "docs/../../outside/secret.txt",
        "docs/../notes.md",
        "..",
    ] {
        assert_eq!(sb.read(path), Err(FsError::ParentDir), "{path}");
        assert_eq!(sb.write(path, b"x"), Err(FsError::ParentDir), "{path}");
        assert_eq!(sb.delete(path), Err(FsError::ParentDir), "{path}");
    }
    assert_eq!(sb.read("./notes.md").expect("dot is fine"), b"my notes");
    secret_is_untouched(&f);
}

#[test]
fn tp_fs_02_absolute_paths_work_only_inside_a_root_and_home_shortcuts_are_refused() {
    let f = Folders::new();
    let sb = f.sandbox();
    let canonical = std::fs::canonicalize(f.root()).expect("canonical");
    let inside = canonical.join("notes.md");
    assert_eq!(
        sb.read(inside.to_str().expect("utf-8")).expect("inside"),
        b"my notes"
    );
    let outside = std::fs::canonicalize(f.outside())
        .expect("canonical")
        .join("secret.txt");
    assert_eq!(
        sb.read(outside.to_str().expect("utf-8")),
        Err(FsError::Outside)
    );
    assert_eq!(sb.write("/etc/hosts", b"x"), Err(FsError::Outside));
    assert_eq!(sb.read("~/.ssh/id_ed25519"), Err(FsError::HomeShortcut));
    assert_eq!(sb.read("~"), Err(FsError::HomeShortcut));
    for bad in ["", "a\0b"] {
        assert!(matches!(sb.read(bad), Err(FsError::BadPath(_))), "{bad:?}");
    }
    let long = "a/".repeat(600);
    assert!(matches!(sb.read(&long), Err(FsError::BadPath(_))));
    let at_cap = "a".repeat(corral_tools::fs::MAX_PATH_BYTES);
    assert!(
        !matches!(sb.read(&at_cap), Err(FsError::BadPath(_))),
        "exactly 1,024 bytes is a usable path"
    );
}

// ---- TP-FS-03..05: links ----------------------------------------------------------------------

#[test]
fn tp_fs_03_a_link_that_leads_outside_is_refused() {
    let f = Folders::new();
    symlink(
        f.outside().join("secret.txt"),
        f.root().join("shortcut.txt"),
    )
    .expect("file link");
    symlink(f.outside(), f.root().join("out")).expect("folder link");
    symlink("../outside/secret.txt", f.root().join("relative.txt")).expect("relative link");
    let sb = f.sandbox();
    for path in ["shortcut.txt", "out/secret.txt", "relative.txt"] {
        assert_eq!(sb.read(path), Err(FsError::Escapes), "{path}");
    }
    assert_eq!(sb.write("out/new.txt", b"x"), Err(FsError::Escapes));
    assert_eq!(
        sb.write("out/secret.txt", b"overwritten"),
        Err(FsError::Escapes)
    );
    assert_eq!(sb.delete("out/secret.txt"), Err(FsError::Escapes));
    assert_eq!(
        sb.write("shortcut.txt", b"overwritten"),
        Err(FsError::NotAFile),
        "writing onto a link is refused, not followed"
    );
    secret_is_untouched(&f);
    assert_eq!(
        f.entries(&f.outside()),
        ["secret.txt"],
        "nothing was created outside"
    );
}

#[test]
fn tp_fs_04_a_link_that_stays_inside_works() {
    let f = Folders::new();
    symlink("notes.md", f.root().join("docs-link.md")).expect("link");
    symlink("docs", f.root().join("papers")).expect("folder link");
    std::fs::write(f.root().join("docs/a.md"), "a").expect("a");
    let sb = f.sandbox();
    assert_eq!(sb.read("docs-link.md").expect("inside"), b"my notes");
    assert_eq!(sb.read("papers/a.md").expect("inside"), b"a");
    symlink(f.root().join("notes.md"), f.root().join("absolute.md")).expect("absolute link");
    assert_eq!(
        sb.read("absolute.md"),
        Err(FsError::Escapes),
        "a link with an absolute target is refused even when it points inside"
    );
}

#[test]
fn tp_fs_05_a_file_also_linked_from_elsewhere_is_refused() {
    let f = Folders::new();
    std::fs::hard_link(f.outside().join("secret.txt"), f.root().join("copy.txt"))
        .expect("hard link");
    let sb = f.sandbox();
    assert_eq!(sb.read("copy.txt"), Err(FsError::Linked));
    assert_eq!(sb.write("copy.txt", b"overwritten"), Err(FsError::Linked));
    assert_eq!(sb.delete("copy.txt"), Err(FsError::Linked));
    secret_is_untouched(&f);
}

// ---- TP-FS-06..07: names APFS treats as the same file ----------------------------------------

#[test]
fn tp_fs_06_a_name_that_differs_only_in_case_is_refused() {
    let f = Folders::new();
    std::fs::write(f.root().join("Report.md"), "v1").expect("report");
    let sb = f.sandbox();
    assert_eq!(
        sb.write("report.md", b"v2"),
        Err(FsError::NameClash {
            existing: "Report.md".into()
        })
    );
    assert_eq!(
        sb.write("docs/../REPORT.md", b"v2"),
        Err(FsError::ParentDir),
        "the path checks come first"
    );
    sb.write("Report.md", b"v2")
        .expect("the exact name is fine");
    assert_eq!(
        std::fs::read_to_string(f.root().join("Report.md")).expect("read"),
        "v2"
    );
}

#[test]
fn tp_fs_07_a_name_that_differs_only_in_unicode_normalization_is_refused() {
    let f = Folders::new();
    let nfc = "caf\u{e9}.md";
    let nfd = "cafe\u{301}.md";
    std::fs::write(f.root().join(nfc), "menu").expect("nfc");
    let sb = f.sandbox();
    if f.entries(&f.root()).iter().any(|n| n == nfc) {
        assert!(
            matches!(sb.write(nfd, b"x"), Err(FsError::NameClash { .. })),
            "NFD over NFC"
        );
    }
    assert!(
        matches!(
            sb.write("CAFE\u{301}.MD", b"x"),
            Err(FsError::NameClash { .. })
        ),
        "case and normalization together"
    );
    assert_eq!(
        std::fs::read_to_string(f.root().join(nfc)).expect("read"),
        "menu"
    );
}

// ---- TP-FS-08: checks happen when the file is opened, not when the sandbox is made ------------

#[test]
fn tp_fs_08_a_folder_swapped_for_a_link_after_the_sandbox_was_made_is_still_refused() {
    let f = Folders::new();
    std::fs::write(f.root().join("docs/plan.md"), "plan").expect("plan");
    let sb = f.sandbox();
    assert_eq!(sb.read("docs/plan.md").expect("before"), b"plan");
    std::fs::remove_dir_all(f.root().join("docs")).expect("remove");
    symlink(f.outside(), f.root().join("docs")).expect("swap in a link");
    std::fs::write(f.outside().join("plan.md"), "planted").expect("plant");
    assert_eq!(sb.read("docs/plan.md"), Err(FsError::Escapes));
    assert_eq!(sb.write("docs/plan.md", b"x"), Err(FsError::Escapes));
    assert_eq!(
        std::fs::read_to_string(f.outside().join("plan.md")).expect("read"),
        "planted"
    );
}

// ---- TP-FS-09: what's read and written ---------------------------------------------------------

#[test]
fn tp_fs_09_writes_are_whole_or_nothing_and_sizes_are_capped() {
    let f = Folders::new();
    let sb = f.sandbox();
    sb.write("docs/new.md", b"hello").expect("write");
    assert_eq!(
        std::fs::read_to_string(f.root().join("docs/new.md")).expect("read"),
        "hello"
    );
    sb.write("docs/new.md", b"again").expect("overwrite");
    assert_eq!(
        f.entries(&f.root().join("docs")),
        ["new.md"],
        "no temp file left behind"
    );

    let at_cap = vec![b'a'; usize::try_from(MAX_WRITE_BYTES).expect("fits")];
    sb.write("big.txt", &at_cap).expect("exactly the cap");
    let mut over = at_cap.clone();
    over.push(b'a');
    assert_eq!(
        sb.write("bigger.txt", &over),
        Err(FsError::TooLarge(MAX_WRITE_BYTES + 1))
    );
    assert!(!f.root().join("bigger.txt").exists());

    let read_cap = usize::try_from(MAX_READ_BYTES).expect("fits");
    std::fs::write(f.root().join("huge.txt"), vec![b'a'; read_cap + 1]).expect("huge");
    assert_eq!(
        sb.read("huge.txt"),
        Err(FsError::TooLarge(MAX_READ_BYTES + 1))
    );
    std::fs::write(f.root().join("fits.txt"), vec![b'a'; read_cap]).expect("fits");
    assert_eq!(
        sb.read("fits.txt").expect("exactly the cap").len(),
        read_cap
    );

    assert_eq!(sb.read("missing.md"), Err(FsError::NotFound));
    assert_eq!(
        sb.write("no-such-folder/x.md", b"x"),
        Err(FsError::NotFound)
    );
    assert_eq!(sb.read("docs"), Err(FsError::NotAFile));
    assert_eq!(sb.delete("docs"), Err(FsError::NotAFile));
    assert_eq!(sb.write("docs", b"x"), Err(FsError::NotAFile));
    sb.delete("docs/new.md").expect("delete");
    assert!(!f.root().join("docs/new.md").exists());
}

#[test]
fn tp_fs_09_roots_must_be_folders_and_a_second_root_is_reached_by_its_absolute_path() {
    let f = Folders::new();
    assert_eq!(
        Sandbox::new(Vec::<PathBuf>::new()).err(),
        Some(FsError::NoRoots)
    );
    assert!(matches!(
        Sandbox::new([f.root().join("notes.md")]),
        Err(FsError::BadRoot(_))
    ));
    assert!(matches!(
        Sandbox::new([f.base.join("missing")]),
        Err(FsError::BadRoot(_))
    ));
    let sb = Sandbox::new([f.root().join("docs"), f.outside()]).expect("two roots");
    let second = std::fs::canonicalize(f.outside())
        .expect("canonical")
        .join("secret.txt");
    assert_eq!(
        sb.read(second.to_str().expect("utf-8"))
            .expect("second root"),
        b"the secret"
    );
    assert_eq!(
        sb.read("secret.txt"),
        Err(FsError::NotFound),
        "relative paths use the first root"
    );
}

// ---- TP-FS-10: the tools -----------------------------------------------------------------------

#[tokio::test]
async fn tp_fs_10_the_file_tools_carry_their_class_and_destination_and_report_plainly() {
    let f = Folders::new();
    let sb = Arc::new(f.sandbox());
    let read = FsRead::new(Arc::clone(&sb));
    let write = FsWrite::new(Arc::clone(&sb));
    let delete = FsDelete::new(Arc::clone(&sb));
    assert_eq!(
        (read.name(), read.class()),
        ("fs.read", ActionClass::ReadLocal)
    );
    assert_eq!(
        (write.name(), write.class()),
        ("fs.write", ActionClass::WriteLocal)
    );
    assert_eq!(
        (delete.name(), delete.class()),
        ("fs.delete", ActionClass::DeleteLocal)
    );
    assert_eq!(
        write.destination(&json!({"path": "docs/a.md", "content": "x"})),
        Some("docs/a.md".into())
    );
    assert_eq!(
        read.untrusted_source(&json!({"path": "notes.md"})),
        None,
        "the agent's own folder"
    );
    assert_eq!(
        read.destination(&json!({"path": "notes.md"})),
        Some("notes.md".into())
    );
    assert_eq!(
        delete.destination(&json!({"path": "old.md"})),
        Some("old.md".into())
    );

    assert_eq!(
        read.call(&json!({"path": "notes.md"})).await,
        Ok("my notes".into())
    );
    assert_eq!(
        write
            .call(&json!({"path": "docs/a.md", "content": "written"}))
            .await,
        Ok("Wrote 7 bytes to docs/a.md.".into())
    );
    assert_eq!(
        delete.call(&json!({"path": "docs/a.md"})).await,
        Ok("Deleted docs/a.md.".into())
    );
    assert!(matches!(
        read.call(&json!({})).await,
        Err(ToolError::BadArgs(_))
    ));
    assert!(matches!(
        write.call(&json!({"path": "a.md"})).await,
        Err(ToolError::BadArgs(_))
    ));
    assert!(matches!(
        read.call(&json!({"path": 7})).await,
        Err(ToolError::BadArgs(_))
    ));
    let Err(ToolError::Failed(why)) = read.call(&json!({"path": "../outside/secret.txt"})).await
    else {
        panic!("refused");
    };
    assert_eq!(why, FsError::ParentDir.to_string());
    std::fs::write(f.root().join("binary.bin"), [0xff, 0xfe, 0x00]).expect("binary");
    assert!(matches!(
        read.call(&json!({"path": "binary.bin"})).await,
        Err(ToolError::Failed(_))
    ));
}

#[test]
fn tp_fs_10_errors_read_plainly() {
    assert_eq!(
        FsError::Escapes.to_string(),
        "That path leads outside the agent's folder through a link, so Corral refused it."
    );
    assert_eq!(
        FsError::Linked.to_string(),
        "That file is also linked from somewhere else, so Corral won't touch it."
    );
    assert_eq!(
        FsError::NameClash {
            existing: "Report.md".into()
        }
        .to_string(),
        "Report.md already exists with a name that differs only in case or accents; use that exact name."
    );
}

#[test]
fn tp_fs_09_writes_at_the_same_time_never_collide() {
    let f = Folders::new();
    let sb = Arc::new(f.sandbox());
    let writers: Vec<_> = (0..8)
        .map(|i| {
            let sb = Arc::clone(&sb);
            std::thread::spawn(move || sb.write(&format!("docs/{i}.md"), b"x"))
        })
        .collect();
    for w in writers {
        w.join().expect("thread").expect("each write lands");
    }
    assert_eq!(
        f.entries(&f.root().join("docs")).len(),
        8,
        "eight files, no temp files"
    );
}

#[test]
fn tp_fs_09_an_unreadable_file_is_not_called_an_escape() {
    use std::os::unix::fs::PermissionsExt;
    let f = Folders::new();
    let locked = f.root().join("locked.md");
    std::fs::write(&locked, "locked").expect("write");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    if std::fs::read(&locked).is_ok() {
        // Running as root (some CI containers): permissions aren't enforced, nothing to check.
        return;
    }
    let refused = f.sandbox().read("locked.md");
    assert!(matches!(refused, Err(FsError::Io(_))), "{refused:?}");
}
