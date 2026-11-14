#![allow(clippy::disallowed_methods)] // tests build and change fixture trees (DD-006)

mod common;

use std::collections::BTreeMap;

use common::*;
use lichtbild::config::Settings;
use lichtbild::scan::Options;

// rivet: verifies REQ-SCAN-001
#[test]
fn scan_never_changes_the_source() {
    let fx = fixture();
    let before1 = snapshot(&fx.root1);
    let before2 = snapshot(&fx.root2);
    let outside = snapshot(&fx.outside);
    let rep = scan_once(&config(&fx, &fx.data), &Options::default());
    assert!(rep.reconciled, "{}", rep.to_text());
    assert_eq!(snapshot(&fx.root1), before1);
    assert_eq!(snapshot(&fx.root2), before2);
    assert_eq!(snapshot(&fx.outside), outside);

    // Again with the roots made read-only: the scan needs no write access.
    set_readonly(&fx.root1, true);
    set_readonly(&fx.root2, true);
    let data2 = fx.data.with_file_name("data-ro");
    let rep = std::panic::catch_unwind(|| scan_once(&config(&fx, &data2), &Options::default()));
    set_readonly(&fx.root1, false);
    set_readonly(&fx.root2, false);
    let rep = rep.unwrap();
    assert!(rep.reconciled, "{}", rep.to_text());
    assert_eq!(rep.roots[0].indexed, 4);
}

// rivet: verifies REQ-SCAN-002
// rivet: verifies REQ-SCAN-005
#[test]
fn report_reconciles_and_names_every_skip() {
    let fx = fixture();
    let rep = scan_once(&config(&fx, &fx.data), &Options::default());
    let p1 = &rep.roots[0];
    assert_eq!(
        (p1.found, p1.indexed, p1.skipped),
        (13, 4, 9),
        "{}",
        rep.to_text()
    );
    assert_eq!(
        (rep.total_found, rep.total_indexed, rep.total_skipped),
        (14, 5, 9)
    );
    let want: BTreeMap<String, u64> = [
        ("broken symlink", 1),
        ("decode error", 1),
        ("hidden or system file", 1),
        ("not yet supported (heic)", 1),
        ("not yet supported (video)", 1),
        ("over size limit", 1),
        ("symlink outside root", 1),
        ("symlink to directory", 1),
        ("unsupported type (.txt)", 1),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    assert_eq!(p1.skipped_by_reason, want);
    assert_eq!(p1.live_in_index, 4);
    assert!(p1.reconciled && rep.reconciled && rep.complete);
    assert_eq!((rep.roots[1].found, rep.roots[1].indexed), (1, 1));

    // The report names counts, never paths (REQ-SCAN-002, SSC-006).
    let json = std::fs::read_to_string(fx.data.join("scan-report.json")).unwrap();
    for needle in [fx.root1.to_str().unwrap(), "IMG_", "secret", "plain"] {
        assert!(!json.contains(needle), "report contains {needle:?}");
        assert!(
            !rep.to_text().contains(needle),
            "text report contains {needle:?}"
        );
    }
}

// rivet: verifies REQ-SCAN-003
#[test]
fn rescan_is_idempotent_and_tracks_changes() {
    let fx = fixture();
    let cfg = config(&fx, &fx.data);
    scan_once(&cfg, &Options::default());
    let first = projection(&fx.data);
    let rep = scan_once(&cfg, &Options::default());
    assert_eq!(projection(&fx.data), first, "second scan changed the index");
    assert_eq!(
        (
            rep.roots[0].new,
            rep.roots[0].updated,
            rep.roots[0].unchanged
        ),
        (0, 0, 4)
    );
    assert_eq!(rep.roots[0].thumbnails.generated, 0);

    // A replaced file is updated; a removed one is marked missing.
    std::fs::write(fx.root1.join("a/IMG_20200102_030405.jpg"), jpeg(50, 30, 9)).unwrap();
    std::fs::remove_file(fx.root1.join("a/IMG_exif.jpg")).unwrap();
    let rep = scan_once(&cfg, &Options::default());
    let p1 = &rep.roots[0];
    assert_eq!(
        (p1.updated, p1.marked_missing, p1.live_in_index),
        (1, 1, 3),
        "{}",
        rep.to_text()
    );
    assert!(rep.reconciled, "{}", rep.to_text());
    let proj = projection(&fx.data);
    assert!(
        proj["p1:a/IMG_exif.jpg"].ends_with("|1"),
        "not marked missing"
    );

    // It comes back: live again.
    std::fs::write(
        fx.root1.join("a/IMG_exif.jpg"),
        jpeg_with_exif(
            "2021:06:15 10:11:12",
            "+02:00",
            Some(((10, 30, 'N'), (20, 15, 'E'))),
            1,
        ),
    )
    .unwrap();
    let rep = scan_once(&cfg, &Options::default());
    assert_eq!(rep.roots[0].live_in_index, 4);
    assert!(rep.reconciled);
}

// rivet: verifies REQ-SCAN-003
// rivet: verifies REQ-THUMB-001
#[test]
fn interrupted_scan_resumes_to_the_same_index() {
    let fx = fixture();
    let a = fx.data.with_file_name("data-a");
    let b = fx.data.with_file_name("data-b");
    // A stale temporary file, as a killed run leaves it.
    std::fs::create_dir_all(a.join("thumbs/00")).unwrap();
    std::fs::write(a.join("thumbs/00/.tmp-1-1"), b"partial").unwrap();

    let cfg_a = config(&fx, &a);
    let rep = scan_once(
        &cfg_a,
        &Options {
            stop_after: Some(2),
        },
    );
    assert!(!rep.complete && !rep.reconciled);
    let rep = scan_once(&cfg_a, &Options::default());
    assert!(rep.complete && rep.reconciled, "{}", rep.to_text());

    scan_once(&config(&fx, &b), &Options::default());
    assert_eq!(projection(&a), projection(&b));
    assert!(!a.join("thumbs/00/.tmp-1-1").exists());

    let list = |d: &std::path::Path| {
        let mut v: Vec<String> = Vec::new();
        for e in std::fs::read_dir(d.join("thumbs")).unwrap() {
            for f in std::fs::read_dir(e.unwrap().path()).unwrap() {
                v.push(f.unwrap().file_name().to_string_lossy().into_owned());
            }
        }
        v.sort();
        v
    };
    assert_eq!(list(&a), list(&b));
    // 5 items, 4 distinct contents (link_in.png is plain.png).
    assert_eq!(list(&b).len(), 4);
    assert!(list(&b).iter().all(|n| n.ends_with(".jpg")));
}

// rivet: verifies REQ-CFG-002
#[test]
fn data_dir_overlapping_a_root_is_refused() {
    let fx = fixture();
    let try_cfg = |data: std::path::PathBuf, roots: Vec<(String, std::path::PathBuf)>| {
        Settings {
            roots,
            data_dir: Some(data),
            ..Settings::default()
        }
        .resolve()
    };
    let p1 = || vec![("p1".to_string(), fx.root1.clone())];

    // A data directory that does not exist is refused, not created.
    let missing = fx.data.with_file_name("not-there");
    let e = try_cfg(missing.clone(), p1()).unwrap_err().to_string();
    assert!(e.contains("does not exist"), "{e}");
    assert!(!missing.exists(), "created a missing data directory");

    // Review finding D1: a not-yet-existing directory under a symlink into
    // a root must be refused without creating anything inside the root.
    let via = fx.data.with_file_name("via-link");
    std::os::unix::fs::symlink(&fx.root1, &via).unwrap();
    assert!(
        try_cfg(via.join("cache"), p1()).is_err(),
        "accepted a path through a symlink into a root"
    );
    assert!(
        !fx.root1.join("cache").exists(),
        "created a directory inside a root"
    );

    let inside = fx.root1.join("cache");
    let e = try_cfg(inside.clone(), p1()).unwrap_err().to_string();
    assert!(e.contains("overlap"), "{e}");
    assert!(!inside.exists(), "created a directory inside a root");

    let e = try_cfg(fx.root1.join("a/../b/../x"), p1())
        .unwrap_err()
        .to_string();
    assert!(e.contains("overlap"), "{e}");
    assert!(!fx.root1.join("x").exists());

    let parent = fx.root1.parent().unwrap().to_path_buf();
    assert!(
        try_cfg(parent, p1()).is_err(),
        "root inside data dir accepted"
    );

    // A symlink that leads into a root.
    let link = fx.data.with_file_name("data-link");
    std::os::unix::fs::symlink(fx.root1.join("a"), &link).unwrap();
    assert!(
        try_cfg(link, p1()).is_err(),
        "symlinked data dir into a root accepted"
    );

    let two = vec![
        ("p1".to_string(), fx.root1.clone()),
        ("p2".to_string(), fx.root1.join("a")),
    ];
    assert!(
        try_cfg(fx.data.clone(), two).is_err(),
        "nested roots accepted"
    );

    std::fs::create_dir_all(&fx.data).unwrap();
    assert!(try_cfg(fx.data.clone(), p1()).is_ok());
}

// rivet: verifies REQ-META-001
#[test]
fn dates_and_positions_carry_their_source() {
    let fx = fixture();
    scan_once(&config(&fx, &fx.data), &Options::default());
    let p = projection(&fx.data);
    let exif = &p["p1:a/IMG_exif.jpg"];
    assert!(
        exif.contains("|2021-06-15T10:11:12|Some(\"+02:00\")|exif|Some(10.5)|Some(20.25)|"),
        "{exif}"
    );
    let named = &p["p1:a/IMG_20200102_030405.jpg"];
    assert!(
        named.contains("|2020-01-02T03:04:05|None|file-name|None|None|"),
        "{named}"
    );
    let plain = &p["p1:a/plain.png"];
    assert!(plain.contains("|file-time|None|None|"), "{plain}");
}

// rivet: verifies REQ-SCAN-004
#[test]
fn scan_lowers_its_priority() {
    let fx = fixture();
    let rep = scan_once(&config(&fx, &fx.data), &Options::default());
    assert!(rep.priority.starts_with("nice 19,"), "{}", rep.priority);
    // SAFETY: getpriority(2) on the current process.
    assert_eq!(unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) }, 19);
    assert_eq!(rep.workers, 2);
}

// rivet: verifies REQ-IDX-001
#[test]
fn one_process_owns_the_index() {
    let fx = fixture();
    let cfg = config(&fx, &fx.data);
    let held = lichtbild::index::Index::open(&cfg.data_dir).unwrap();
    let err = lichtbild::index::Index::open(&cfg.data_dir)
        .err()
        .expect("second open of the index succeeded");
    assert!(err.to_string().contains("in use"), "{err}");
    drop(held);
    assert!(lichtbild::index::Index::open(&cfg.data_dir).is_ok());
}

// rivet: verifies REQ-THUMB-001
#[test]
fn failed_thumbnails_are_retried_and_counted() {
    let fx = fixture();
    let mut cfg = config(&fx, &fx.data);
    cfg.limits.thumb_cache_bytes = 0; // cache full from the start
    let rep = scan_once(&cfg, &Options::default());
    assert!(rep.reconciled, "{}", rep.to_text());
    assert_eq!(rep.roots[0].thumbnails.failed.get("cache-full"), Some(&4));
    // The next scan still reports them.
    let rep = scan_once(&cfg, &Options::default());
    assert_eq!(
        rep.roots[0].thumbnails.failed.get("cache-full"),
        Some(&4),
        "{}",
        rep.to_text()
    );
    // With room in the cache they are generated.
    cfg.limits.thumb_cache_bytes = 1 << 30;
    let rep = scan_once(&cfg, &Options::default());
    assert_eq!(
        rep.roots[0].thumbnails.failed.get("cache-full"),
        None,
        "{}",
        rep.to_text()
    );
    assert_eq!(
        rep.roots[0].thumbnails.generated + rep.roots[0].thumbnails.reused,
        4
    );
    assert_eq!(rep.roots[0].unchanged, 0, "retried items count as updated");
}
