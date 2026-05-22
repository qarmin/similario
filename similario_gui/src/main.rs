// Slint-generated code triggers these lints; suppress at crate level.
#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::todo,
    reason = "generated Slint code"
)]

slint::include_modules!();

mod callbacks;
mod format;
mod rows;
mod settings;

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use settings::GuiSettings;
use slint::{ComponentHandle, ModelRc, VecModel};

fn setup_logger() {
    let config = handsome_logger::ConfigBuilder::default()
        .set_level(log::LevelFilter::Info)
        .set_message_filtering(Some(|record: &log::Record| {
            record.module_path().is_none_or(|m| m.starts_with("similario"))
        }))
        .build();
    let _ = handsome_logger::TermLogger::init(
        config,
        handsome_logger::TerminalMode::Mixed,
        handsome_logger::ColorChoice::Auto,
    );
}

fn load_settings_to_window(window: &MainWindow, s: &GuiSettings) {
    window.set_cfg_tolerance(s.tolerance);
    window.set_cfg_duration_tolerance_pct(s.duration_tolerance_pct);
    window.set_cfg_min_matching_windows(s.min_matching_windows);
    window.set_cfg_subclip_min_match(s.subclip_min_match);
    window.set_cfg_window_count(s.window_count);
    window.set_cfg_skip_secs(s.skip_secs);
    window.set_cfg_cropdetect(s.cropdetect);
    window.set_cfg_audio_fingerprint(s.audio_fingerprint);
    window.set_cfg_audio_max_difference(s.audio_max_difference);
    window.set_cfg_audio_min_segment_duration(s.audio_min_segment_duration);
    window.global::<Settings>().set_dark_theme(s.dark_theme);
    if !s.last_directory.is_empty() {
        window.set_dir_path(s.last_directory.as_str().into());
    }
}

fn save_settings_from_window(window: &MainWindow) {
    let s = GuiSettings {
        tolerance: window.get_cfg_tolerance(),
        duration_tolerance_pct: window.get_cfg_duration_tolerance_pct(),
        min_matching_windows: window.get_cfg_min_matching_windows(),
        subclip_min_match: window.get_cfg_subclip_min_match(),
        window_count: window.get_cfg_window_count(),
        skip_secs: window.get_cfg_skip_secs(),
        cropdetect: window.get_cfg_cropdetect(),
        audio_fingerprint: window.get_cfg_audio_fingerprint(),
        audio_max_difference: window.get_cfg_audio_max_difference(),
        audio_min_segment_duration: window.get_cfg_audio_min_segment_duration(),
        last_directory: window.get_dir_path().to_string(),
        dark_theme: window.global::<Settings>().get_dark_theme(),
    };
    s.save();
}

fn main() {
    setup_logger();

    let window = MainWindow::new().expect("Failed to create window");

    // Load persisted settings
    let settings = GuiSettings::load();
    load_settings_to_window(&window, &settings);

    let rows_data: Rc<VecModel<FileRow>> = Rc::new(VecModel::default());
    window.set_rows(ModelRc::from(rows_data.clone()));

    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_and_compare_flag = Arc::new(AtomicBool::new(false));

    callbacks::bind_stop(&window, &stop_flag);
    callbacks::bind_stop_and_compare(&window, &stop_flag, &stop_and_compare_flag);
    callbacks::bind_select_all(&window, &rows_data);
    callbacks::bind_check_toggled(&window, &rows_data);
    callbacks::bind_row_double_clicked(&window, &rows_data);
    callbacks::bind_request_delete(&window, &rows_data);
    callbacks::bind_request_trash(&window, &rows_data);
    callbacks::bind_delete_selected(&window, &rows_data);
    callbacks::bind_trash_selected(&window, &rows_data);
    callbacks::bind_scan(&window, &stop_flag, &stop_and_compare_flag);

    // CLI arg overrides persisted directory
    if let Some(dir) = std::env::args().nth(1) {
        window.set_dir_path(dir.into());
    }

    window.run().expect("Failed to run window");

    // Save settings on exit
    save_settings_from_window(&window);
}
