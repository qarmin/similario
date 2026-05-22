pub fn format_size(b: u64) -> String {
    humansize::format_size(b, humansize::BINARY)
}

pub fn format_duration(secs: f64) -> String {
    let t = secs as u64;
    let h = t / 3600;
    let m = (t % 3600) / 60;
    let s = t % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub fn format_bitrate(bps: u64) -> String {
    if bps >= 1024 * 1024 {
        format!("{:.1} Mb/s", bps as f64 / (1024 * 1024) as f64)
    } else {
        format!("{} kb/s", bps / 1024)
    }
}

pub fn format_date(secs: u64) -> String {
    use chrono::{Local, TimeZone, Utc};
    let dt_local = Utc
        .timestamp_opt(secs as i64, 0)
        .single()
        .unwrap_or_default()
        .with_timezone(&Local);
    dt_local.format("%Y-%m-%d %H:%M:%S").to_string()
}
