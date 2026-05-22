use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;
use similario_core::compare::{CompareConfig, find_similar};
use similario_core::thumbnail::extract_thumbnail;
use similario_core::{
    ScanOutcome, SignatureConfig, VideoSignature, check_cache, collect_video_files, compute_signatures,
};
use slint::{ComponentHandle, Image, Model, Rgb8Pixel, SharedPixelBuffer, SharedString, VecModel};

use crate::rows::{PlainRow, groups_to_plain_rows, plain_to_file_row, with_vec_model};
use crate::{FileRow, MainWindow};

const THUMB_SEEK_PCT: f32 = 0.20;
const THUMB_MAX_W: u32 = 224;
const THUMB_MAX_H: u32 = 126;

pub fn bind_stop(window: &MainWindow, stop_flag: &Arc<AtomicBool>) {
    let flag = stop_flag.clone();
    window.on_stop_clicked(move || {
        flag.store(true, Ordering::SeqCst);
    });
}

pub fn bind_stop_and_compare(
    window: &MainWindow,
    stop_flag: &Arc<AtomicBool>,
    stop_and_compare_flag: &Arc<AtomicBool>,
) {
    let stop = stop_flag.clone();
    let compare = stop_and_compare_flag.clone();
    window.on_stop_and_compare_clicked(move || {
        compare.store(true, Ordering::SeqCst);
        stop.store(true, Ordering::SeqCst);
    });
}

pub fn bind_select_all(window: &MainWindow, rows_data: &Rc<VecModel<FileRow>>) {
    use crate::QualityTier;

    let data = rows_data.clone();
    window.on_select_all(move |checked| {
        let mut rows: Vec<FileRow> = data.iter().collect();
        if !checked {
            for r in &mut rows {
                if !r.is_header {
                    r.checked = false;
                }
            }
            data.set_vec(rows);
            return;
        }

        // Walk groups (delimited by header rows) and within each group leave
        // exactly one row unchecked: the Best tier if present, otherwise the
        // first member. Groups with a single member stay fully unchecked.
        let mut i = 0;
        while i < rows.len() {
            if !rows[i].is_header {
                i += 1;
                continue;
            }
            let group_start = i + 1;
            let mut group_end = group_start;
            while group_end < rows.len() && !rows[group_end].is_header {
                group_end += 1;
            }
            let group = group_start..group_end;
            let size = group.len();
            if size >= 2 {
                let keep = group
                    .clone()
                    .find(|&j| rows[j].quality_tier == QualityTier::Best)
                    .unwrap_or(group_start);
                for j in group {
                    rows[j].checked = j != keep;
                }
            } else {
                for j in group {
                    rows[j].checked = false;
                }
            }
            i = group_end;
        }
        data.set_vec(rows);
    });
}

pub fn bind_check_toggled(window: &MainWindow, rows_data: &Rc<VecModel<FileRow>>) {
    let data = rows_data.clone();
    window.on_check_toggled(move |idx, checked| {
        if let Some(mut r) = data.row_data(idx as usize) {
            r.checked = checked;
            data.set_row_data(idx as usize, r);
        }
    });
}

pub fn bind_row_double_clicked(window: &MainWindow, rows_data: &Rc<VecModel<FileRow>>) {
    let data = rows_data.clone();
    window.on_row_double_clicked(move |idx| {
        if let Some(r) = data.row_data(idx as usize)
            && !r.is_header
            && !r.path.is_empty()
        {
            let path = r.path.to_string();
            std::thread::spawn(move || {
                if let Err(e) = std::process::Command::new("xdg-open").arg(&path).spawn() {
                    log::warn!("Cannot open {path}: {e}");
                }
            });
        }
    });
}

fn checked_count(data: &Rc<VecModel<FileRow>>) -> usize {
    data.iter().filter(|r| r.checked && !r.is_header).count()
}

fn remove_checked_rows(data: &Rc<VecModel<FileRow>>) {
    let kept: Vec<FileRow> = data.iter().filter(|r| !r.checked || r.is_header).collect();
    data.set_vec(kept);
}

fn collect_checked_paths(data: &Rc<VecModel<FileRow>>) -> Vec<String> {
    data.iter()
        .filter(|r| r.checked && !r.is_header)
        .map(|r| r.path.to_string())
        .collect()
}

pub fn bind_request_delete(window: &MainWindow, rows_data: &Rc<VecModel<FileRow>>) {
    let data = rows_data.clone();
    let weak = window.as_weak();
    window.on_request_delete(move || {
        let count = checked_count(&data);
        if count == 0 {
            return;
        }
        let w = weak.upgrade().expect("MainWindow alive during request-delete");
        w.set_delete_confirmation_text(SharedString::from(format!(
            "Permanently delete {count} file(s)?\nThis cannot be undone."
        )));
        w.invoke_show_delete_popup();
    });
}

pub fn bind_request_trash(window: &MainWindow, rows_data: &Rc<VecModel<FileRow>>) {
    let data = rows_data.clone();
    let weak = window.as_weak();
    window.on_request_trash(move || {
        let count = checked_count(&data);
        if count == 0 {
            return;
        }
        let w = weak.upgrade().expect("MainWindow alive during request-trash");
        w.set_trash_confirmation_text(SharedString::from(format!("Move {count} file(s) to trash?")));
        w.invoke_show_trash_popup();
    });
}

pub fn bind_delete_selected(window: &MainWindow, rows_data: &Rc<VecModel<FileRow>>) {
    let data = rows_data.clone();
    window.on_delete_selected(move || {
        for p in collect_checked_paths(&data) {
            if let Err(e) = std::fs::remove_file(&p) {
                log::warn!("Cannot delete {p}: {e}");
            }
        }
        remove_checked_rows(&data);
    });
}

pub fn bind_trash_selected(window: &MainWindow, rows_data: &Rc<VecModel<FileRow>>) {
    let data = rows_data.clone();
    window.on_trash_selected(move || {
        for p in collect_checked_paths(&data) {
            if let Err(e) = trash::delete(&p) {
                log::warn!("Cannot trash {p}: {e}");
            }
        }
        remove_checked_rows(&data);
    });
}

pub fn bind_scan(window: &MainWindow, stop_flag: &Arc<AtomicBool>, stop_and_compare_flag: &Arc<AtomicBool>) {
    let weak = window.as_weak();
    let stop = stop_flag.clone();
    let compare = stop_and_compare_flag.clone();

    window.on_scan_clicked(move |dir_str| {
        let dir = PathBuf::from(dir_str.as_str());
        if !dir.exists() {
            return;
        }

        stop.store(false, Ordering::SeqCst);
        compare.store(false, Ordering::SeqCst);

        // Read config from UI (must happen on UI thread, before spawn).
        let (sig_config, compare_config) = if let Some(w) = weak.upgrade() {
            let audio_enabled = w.get_cfg_audio_fingerprint();
            let sig = SignatureConfig {
                skip_secs: f64::from(w.get_cfg_skip_secs()),
                window_count: w.get_cfg_window_count() as usize,
                cropdetect: w.get_cfg_cropdetect(),
                audio_fingerprint: audio_enabled,
                ..SignatureConfig::default()
            };
            let cmp = CompareConfig {
                tolerance: w.get_cfg_tolerance(),
                duration_tolerance_pct: w.get_cfg_duration_tolerance_pct() as f64,
                min_matching_windows: w.get_cfg_min_matching_windows(),
                subclip_min_match: w.get_cfg_subclip_min_match(),
                use_audio: audio_enabled,
                audio_max_difference: w.get_cfg_audio_max_difference() as f64,
                audio_min_segment_duration: w.get_cfg_audio_min_segment_duration(),
            };
            (sig, cmp)
        } else {
            (SignatureConfig::default(), CompareConfig::default())
        };

        let _ = weak.upgrade_in_event_loop(|w| {
            with_vec_model(&w.get_rows(), |vm| vm.set_vec(vec![]));
            w.set_scanning(true);
            w.set_scan_progress(0.0);
            w.set_status_text(SharedString::from("Collecting files..."));
        });

        let weak2 = weak.clone();
        let stop2 = stop.clone();
        let compare2 = compare.clone();

        std::thread::spawn(move || {
            run_scan(weak2, stop2, compare2, dir, sig_config, compare_config);
        });
    });
}

#[expect(clippy::needless_pass_by_value, reason = "moved into thread")]
fn run_scan(
    weak: slint::Weak<MainWindow>,
    stop: Arc<AtomicBool>,
    stop_and_compare: Arc<AtomicBool>,
    dir: PathBuf,
    sig_config: SignatureConfig,
    compare_config: CompareConfig,
) {
    let weak_new = weak.clone();
    drop(weak);
    let weak = &weak_new;

    let paths = {
        let weak_collect = weak.clone();
        collect_video_files(&dir, |n| {
            if n % 50 == 0 {
                let msg = SharedString::from(format!("Collecting files... {n}"));
                let _ = weak_collect.upgrade_in_event_loop(move |w| {
                    w.set_status_text(msg);
                });
            }
        })
    };
    let total = paths.len();

    if paths.is_empty() {
        let _ = weak.upgrade_in_event_loop(|w| {
            w.set_scanning(false);
            w.set_status_text(SharedString::from("No video files found."));
        });
        return;
    }

    set_status(weak, format!("Checking cache for {total} files..."));

    let cache_result = check_cache(&paths, &sig_config);
    let n_cached = cache_result.cached.len();
    let n_uncached = cache_result.uncached.len();

    set_status(weak, format!("{n_cached} cached, {n_uncached} to process..."));

    let outcomes = {
        let weak3 = weak.clone();
        compute_signatures(&cache_result.uncached, &sig_config, true, &stop, move |done, total| {
            let progress = done as f32 / total as f32;
            let msg = SharedString::from(format!("Processing: {done}/{total} ({n_cached} cached)"));
            let _ = weak3.upgrade_in_event_loop(move |w| {
                w.set_scan_progress(progress);
                w.set_status_text(msg);
            });
        })
    };

    let was_stopped = stop.load(Ordering::SeqCst);
    let should_compare = stop_and_compare.load(Ordering::SeqCst);

    if was_stopped && !should_compare {
        let _ = weak.upgrade_in_event_loop(|w| {
            w.set_scanning(false);
            w.set_status_text(SharedString::from("Stopped."));
        });
        return;
    }

    let mut sigs = cache_result.cached;
    let mut errors = 0usize;
    for outcome in outcomes {
        match outcome {
            ScanOutcome::Computed(s) | ScanOutcome::Cached(s) => sigs.push(s),
            ScanOutcome::Error(_, _) => errors += 1,
        }
    }

    let partial = if was_stopped && should_compare {
        " (partial)"
    } else {
        ""
    };

    set_status(weak, format!("Comparing {} signatures{partial}...", sigs.len()));

    let groups = find_similar(&sigs, &compare_config);
    let n_groups = groups.len();
    let sig_map: HashMap<PathBuf, &VideoSignature> = sigs.iter().map(|s| (s.path.clone(), s)).collect();
    let plain_rows = groups_to_plain_rows(&groups, &sig_map);

    let msg = SharedString::from(format!(
        "Found {n_groups} groups{partial}. Errors: {errors}. Loading thumbnails..."
    ));
    let plain_for_thumbs = plain_rows.clone();

    let _ = weak.upgrade_in_event_loop(move |w| {
        w.set_scanning(false);
        w.set_scan_progress(1.0);
        w.set_status_text(msg);
        let file_rows: Vec<FileRow> = plain_rows.into_iter().map(plain_to_file_row).collect();
        with_vec_model(&w.get_rows(), |vm| vm.set_vec(file_rows));
    });

    load_thumbnails(weak, &plain_for_thumbs);

    let done_msg = SharedString::from(format!("Found {n_groups} groups{partial}. Errors: {errors}."));
    let _ = weak.upgrade_in_event_loop(move |w| {
        w.set_status_text(done_msg);
    });
}

fn load_thumbnails(weak: &slint::Weak<MainWindow>, rows: &[PlainRow]) {
    let thumb_items: Vec<(usize, PathBuf, f32)> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| !row.is_header && !row.path.is_empty())
        .map(|(idx, row)| {
            let path = PathBuf::from(&row.path);
            let seek = (row.duration_secs as f32 * THUMB_SEEK_PCT).max(1.0);
            (idx, path, seek)
        })
        .collect();

    let results: Vec<(usize, u32, u32, Vec<u8>)> = thumb_items
        .par_iter()
        .filter_map(
            |(idx, path, seek)| match extract_thumbnail(path, *seek, THUMB_MAX_W, THUMB_MAX_H) {
                Ok(img) => {
                    let w = img.width();
                    let h = img.height();
                    Some((*idx, w, h, img.into_raw()))
                }
                Err(e) => {
                    log::debug!("Thumbnail failed for {}: {e}", path.display());
                    None
                }
            },
        )
        .collect();

    for (idx, w, h, raw) in results {
        let weak_t = weak.clone();
        let _ = weak_t.upgrade_in_event_loop(move |win| {
            let mut buf = SharedPixelBuffer::<Rgb8Pixel>::new(w, h);
            buf.make_mut_bytes().copy_from_slice(&raw);
            let thumb = Image::from_rgb8(buf);
            with_vec_model(&win.get_rows(), |vm| {
                if let Some(mut r) = vm.row_data(idx) {
                    r.thumbnail = thumb;
                    vm.set_row_data(idx, r);
                }
            });
        });
    }
}

fn set_status(weak: &slint::Weak<MainWindow>, text: String) {
    let msg = SharedString::from(text);
    let _ = weak.upgrade_in_event_loop(move |w| {
        w.set_status_text(msg);
    });
}
