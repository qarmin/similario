# Similario core

Library for detecting similar video files using visual 3D-DCT hashing and its audio.

Part of the [Similario](https://github.com/qarmin/similario) project. The CLI and GUI front-ends are built on top of this crate.

## Requirements

- **Rust** (edition 2024)
- **ffmpeg** and **ffprobe** in `PATH` at runtime (used for frame extraction and metadata)

## How it works

1. Collect video files from a directory (`collect_video_files`).
2. Optionally load cached signatures (`check_cache`).
3. Compute signatures for the rest (`compute_signatures`): ffmpeg extracts 16×16 grayscale frames across N temporal windows, then a 3D-DCT produces a 1000-bit binary hash.
4. Compare signatures with duration bucketing + Hamming distance (`find_similar`).

The returned `SimilarGroup` tells you whether files are `Identical`, `SameContent`, `SubClip`, or merely `Similar`.

## Example

```rust
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use similario_core::compare::{CompareConfig, find_similar};
use similario_core::{
    ScanOutcome, SignatureConfig, check_cache, collect_video_files, compute_signatures,
};

fn main() {
    let dir = PathBuf::from("/path/to/videos");
    let stop = AtomicBool::new(false);

    // 1. Find video files on disk.
    let paths = collect_video_files(&dir, |_| {});

    // 2. Configure how signatures are computed.
    let sig_cfg = SignatureConfig {
        skip_secs: 15.0,
        window_count: 5,
        cropdetect: true,
        ..SignatureConfig::default()
    };

    // 3. Reuse anything already in the cache, compute the rest.
    let cached = check_cache(&paths, &sig_cfg);
    let outcomes = compute_signatures(
        &cached.uncached,
        &sig_cfg,
        true,         // save new results to cache
        &stop,        // cooperative stop flag
        |done, total| println!("{done}/{total}"),
    );

    // 4. Collect all signatures (cached + freshly computed).
    let mut sigs = cached.cached;
    for outcome in outcomes {
        if let ScanOutcome::Computed(s) | ScanOutcome::Cached(s) = outcome {
            sigs.push(s);
        }
    }

    // 5. Find similar groups.
    let cmp_cfg = CompareConfig {
        tolerance: 0.30,
        ..CompareConfig::default()
    };
    for group in find_similar(&sigs, &cmp_cfg) {
        println!("{:?}", group.kind);
        for path in group.files {
            println!("  {}", path.display());
        }
    }
}
```

## License

MIT

## AI Usage
This project was developed with the assistance of AI. 
