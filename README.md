# Similario

Video duplicate detector. Finds identical copies, re-encodes, cropped versions and sub-clips using visual 3D-DCT hashing.

It contains 3 parts, core library, command-line tool, and slint GUI.

The main reason for creating it, was the needing to decrease number of false positives in Czkawka/Krokiet.

## Requirements

- **Rust** (edition 2024)
- **ffmpeg** and **ffprobe** in `PATH` (runtime dependency)

## How It Works

1. **File collection** - recursively finds video files (mp4, mkv, avi, mov, wmv, flv, webm, m4v, mpg, mpeg, ts, mts, m2ts, vob, 3gp, ogv, and more)
2. **Letterbox detection** - optional pre-scan detects black bars and crops them before hashing
3. **Frame extraction** - ffmpeg extracts 16×16 grayscale frames across N temporal windows (default: 5 windows × 16 frames = 80 frames per file)
4. **3D-DCT hashing** - each window's frames form a 16×16×16 tensor → 3D Discrete Cosine Transform → 1000-bit binary hash
5. **Comparison** - duration bucketing (±20%) reduces O(n²), pre-filter on middle window, then full Hamming distance comparison
6. **Sub-clip detection** - sliding window alignment finds temporal offsets
7. **Grouping** - anti-daisy-chain clustering with representative validation

## License

MIT - CLI, Core + Code part of GUI

GPL-3.0 - GUI as a whole (Slint's license requirement)

## AI Usage
This project was developed with the assistance of AI. 
