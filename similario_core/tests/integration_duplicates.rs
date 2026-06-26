#![expect(clippy::unwrap_used, reason = "test code")]
#![expect(clippy::print_stdout, reason = "test diagnostics")]
#![expect(clippy::print_stderr, reason = "test diagnostics")]
//! Integration test: verifies that similario correctly detects
//! duplicate/similar video groups from prepared test videos.
//!
//! Requires `tests/videos/` to be populated first via:
//!   just prepare-test-videos

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use similario_core::compare::{CompareConfig, find_similar};
use similario_core::{SignatureConfig, SimilarityKind, collect_video_files, compute_signatures};

fn videos_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("tests/videos")
}

fn ensure_videos_exist(dir: &Path) {
    assert!(
        dir.exists() && std::fs::read_dir(dir).map_or(0, |d| d.count()) > 0,
        "Test videos not found at {}. Run `just prepare-test-videos` first.",
        dir.display()
    );
}

/// Returns the base ID of a video file (e.g. "D148JNXfXwU" from "D148JNXfXwU_480p.mp4").
fn base_id(path: &Path) -> String {
    let stem = path.file_stem().unwrap().to_str().unwrap();
    // Original files: just the ID. Variants: ID_suffix.
    stem.split('_').next().unwrap().to_string()
}

#[test]
fn test_all_variants_grouped_with_originals() {
    let dir = videos_dir();
    ensure_videos_exist(&dir);

    let paths = collect_video_files(&dir, |_| {});
    assert!(
        paths.len() >= 8,
        "Expected at least 8 video files, found {}",
        paths.len()
    );

    let stop = Arc::new(AtomicBool::new(false));
    let sig_config = SignatureConfig::default();

    let outcomes = compute_signatures(&paths, &sig_config, false, &stop, |_, _| {});

    let sigs: Vec<_> = outcomes
        .into_iter()
        .filter_map(|o| match o {
            similario_core::ScanOutcome::Computed(s) | similario_core::ScanOutcome::Cached(s) => Some(s),
            similario_core::ScanOutcome::Error(p, e) => {
                eprintln!("WARN: failed to process {}: {e}", p.display());
                None
            }
        })
        .collect();

    let compare_config = CompareConfig::default();
    let groups = find_similar(&sigs, &compare_config);

    println!("=== Found {} groups ===", groups.len());
    for (i, group) in groups.iter().enumerate() {
        println!("Group {i}: {:?} - {} files", group.kind, group.files.len());
        for f in &group.files {
            println!("  {}", f.display());
        }
    }

    // Each original video should appear in at least one group
    let originals: Vec<&PathBuf> = paths
        .iter()
        .filter(|p| {
            let stem = p.file_stem().unwrap().to_str().unwrap();
            !stem.contains('_')
        })
        .collect();

    let grouped_paths: HashSet<PathBuf> = groups.iter().flat_map(|g| g.files.iter().cloned()).collect();

    let mut missing_originals = Vec::new();
    for orig in &originals {
        if !grouped_paths.contains(*orig) {
            missing_originals.push(orig.display().to_string());
        }
    }
    assert!(
        missing_originals.is_empty(),
        "These originals were not found in any similarity group: {missing_originals:?}"
    );

    // Each group should contain files sharing the same base ID
    for group in &groups {
        let ids: HashSet<String> = group.files.iter().map(|p| base_id(p)).collect();
        assert_eq!(
            ids.len(),
            1,
            "Group mixes different videos: {ids:?}\nFiles: {:?}",
            group.files
        );
    }

    // Re-encoded / remuxed variants should be Identical or SameContent
    for group in &groups {
        let has_reencode_or_remux = group.files.iter().any(|p| {
            let stem = p.file_stem().unwrap().to_str().unwrap();
            stem.ends_with("_reencode") || stem.ends_with("_remux")
        });
        if has_reencode_or_remux {
            assert!(
                matches!(group.kind, SimilarityKind::Identical | SimilarityKind::SameContent),
                "Re-encoded/remuxed files should be Identical or SameContent, got {:?}\nFiles: {:?}",
                group.kind,
                group.files,
            );
        }
    }

    // Trimmed variants should be SubClip
    for group in &groups {
        let has_trimmed = group.files.iter().any(|p| {
            let stem = p.file_stem().unwrap().to_str().unwrap();
            stem.ends_with("_trimmed")
        });
        if has_trimmed {
            assert!(
                matches!(group.kind, SimilarityKind::SubClip { .. }),
                "Trimmed files should be SubClip, got {:?}\nFiles: {:?}",
                group.kind,
                group.files,
            );
        }
    }

    println!("\n=== All assertions passed ===");
}
