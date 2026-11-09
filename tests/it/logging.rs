//! Tests for `jff_wm::logging`: which file a `path` means and how a log folder is kept tidy.

use std::path::{Path, PathBuf};

use jff_wm::logging::{is_log_dir, log_file_for, prune_sessions};

/// A folder of its own under the system temp dir, empty.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("jff-logging-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_file_path_is_the_file() {
    let path = Path::new("/tmp/jff-no-such-folder/jff.log");
    assert!(!is_log_dir(path));
    assert_eq!(log_file_for(path, "2026-11-09_17-00-12"), path);
}

#[test]
fn a_trailing_slash_or_an_existing_folder_is_a_log_folder() {
    let missing = Path::new("/tmp/jff-no-such-folder/");
    assert!(is_log_dir(missing));
    assert_eq!(
        log_file_for(missing, "2026-11-09_17-00-12"),
        Path::new("/tmp/jff-no-such-folder/jff-2026-11-09_17-00-12.log")
    );

    let dir = scratch("dir");
    assert!(is_log_dir(&dir));
    assert_eq!(
        log_file_for(&dir, "2026-11-09_17-00-12"),
        dir.join("jff-2026-11-09_17-00-12.log")
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn pruning_keeps_the_newest_session_files_and_nothing_else_is_touched() {
    let dir = scratch("prune");
    for day in 1..=5 {
        std::fs::write(dir.join(format!("jff-2026-11-0{day}_10-00-00.log")), "x").unwrap();
    }
    std::fs::write(dir.join("notes.log"), "x").unwrap();
    std::fs::write(dir.join("jff.log"), "x").unwrap();
    std::os::unix::fs::symlink("jff-2026-11-05_10-00-00.log", dir.join("jff-latest.log")).unwrap();

    prune_sessions(&dir, 2, &dir.join("jff-2026-11-05_10-00-00.log"));
    assert_eq!(
        names(&dir),
        [
            "jff-2026-11-04_10-00-00.log",
            "jff-2026-11-05_10-00-00.log",
            "jff-latest.log",
            "jff.log",
            "notes.log",
        ]
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn pruning_never_deletes_the_current_file() {
    let dir = scratch("current");
    for day in 1..=3 {
        std::fs::write(dir.join(format!("jff-2026-11-0{day}_10-00-00.log")), "x").unwrap();
    }
    // The clock went backwards: the current session is the oldest name.
    prune_sessions(&dir, 1, &dir.join("jff-2026-11-01_10-00-00.log"));
    assert!(dir.join("jff-2026-11-01_10-00-00.log").exists());
    let _ = std::fs::remove_dir_all(dir);
}
