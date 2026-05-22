//! Audio fingerprinting via Symphonia + rusty-chromaprint.
//!
//! Decodes the first audio track from a video/audio file and computes a
//! Chromaprint fingerprint (Vec<u32>). The fingerprint can then be compared
//! using `rusty_chromaprint::match_fingerprints`.

use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use rusty_chromaprint::{Configuration, Fingerprinter};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::FormatOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// Computes a Chromaprint fingerprint for the first audio track in the given file.
///
/// Returns `Ok(Some(fingerprint))` on success, `Ok(None)` if stopped, `Err` on failure.
pub fn compute_fingerprint(path: &Path, stop_flag: &AtomicBool) -> Result<Option<Vec<u32>>, String> {
    let config = Configuration::preset_test1();
    compute_fingerprint_with_config(path, &config, stop_flag)
}

fn compute_fingerprint_with_config(
    path: &Path,
    config: &Configuration,
    stop_flag: &AtomicBool,
) -> Result<Option<Vec<u32>>, String> {
    let src = File::open(path).map_err(|e| format!("open: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(src), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(std::ffi::OsStr::to_str) {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|_| "unsupported format".to_string())?;

    let track = format
        .tracks()
        .iter()
        .find(|t| {
            if let Some(CodecParameters::Audio(p)) = t.codec_params.as_ref() {
                p.sample_rate.is_some()
            } else {
                false
            }
        })
        .ok_or_else(|| "no audio track".to_string())?;

    let audio_params = match track.codec_params.as_ref() {
        Some(CodecParameters::Audio(p)) => p.clone(),
        _ => unreachable!(),
    };
    let track_id = track.id;

    let decoder_opts = AudioDecoderOptions::default();
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&audio_params, &decoder_opts)
        .map_err(|_| "unsupported codec".to_string())?;

    let mut printer = Fingerprinter::new(config);
    let mut printer_started = false;
    let mut samples_i16: Vec<i16> = Vec::new();
    let mut total_samples: u64 = 0;
    let mut sum_sq: f64 = 0.0;
    let mut max_amp: f64 = 0.0;

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            return Ok(None);
        }

        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Err(Error::IoError(_) | _) | Ok(None) => break,
        };

        if packet.track_id != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(audio_buf) => {
                let spec = audio_buf.spec();

                if !printer_started {
                    printer
                        .start(spec.rate(), spec.channels().count() as u32)
                        .map_err(|_| "fingerprinter init failed".to_string())?;
                    printer_started = true;
                }

                samples_i16.clear();
                audio_buf.copy_to_vec_interleaved(&mut samples_i16);

                total_samples += samples_i16.len() as u64;
                for &s in &samples_i16 {
                    let v = f64::from(s) / f64::from(i16::MAX);
                    sum_sq += v * v;
                    let a = v.abs();
                    if a > max_amp {
                        max_amp = a;
                    }
                }
                printer.consume(&samples_i16);
            }
            Err(Error::DecodeError(_)) => (),
            Err(_) => break,
        }
    }

    if !printer_started {
        return Err("no audio frames decoded".to_string());
    }

    printer.finish();

    // Silent file → empty fingerprint (skip comparisons but cache the result).
    let rms = if total_samples > 0 {
        (sum_sq / total_samples as f64).sqrt()
    } else {
        0.0
    };
    if rms < 0.001 && max_amp < 0.01 {
        return Ok(Some(vec![]));
    }

    Ok(Some(printer.fingerprint().to_vec()))
}
