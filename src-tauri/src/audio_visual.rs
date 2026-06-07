use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use spectrs::spectrogram::stft::{par_compute_spectrogram, SpectrogramType};

use crate::models::AudioSpectrogram;
use crate::player::ensure_gstreamer;
use crate::process_util::{command_output_with_timeout, YTDLP_TIMEOUT};

const TARGET_SAMPLE_RATE: i32 = 22_050;
const N_FFT: usize = 2_048;
const WIN_LENGTH: usize = 1_024;
const MIN_HOP_LENGTH: usize = 96;
const TARGET_TIME_BINS: usize = 12_288;
const FREQ_BINS: usize = 128;
const DISPLAY_DYNAMIC_RANGE_DB: f32 = 78.0;

pub fn build_timeline_spectrogram(video_id: &str, url: &str) -> Result<AudioSpectrogram> {
    let cache_dir = audio_cache_dir()?;
    fs::create_dir_all(&cache_dir)?;
    let key = safe_cache_key(video_id);
    let audio_path = cached_audio_path(&cache_dir, &key, url)?;
    let samples = decode_audio_mono_f32(&audio_path)?;
    build_spectrogram_from_samples(video_id, &samples)
}

/// Build a spectrogram from the Demucs-separated vocals for this video.
///
/// Reuses a `vocals.wav` from a prior ASR-with-Demucs run if one is still
/// cached; otherwise runs Demucs on the timeline audio (an expensive ML pass)
/// and caches the result alongside it.
pub fn build_timeline_demucs_spectrogram(video_id: &str, url: &str) -> Result<AudioSpectrogram> {
    let vocals = match crate::asr::cached_demucs_vocals(video_id) {
        Some(path) => path,
        None => {
            let cache_dir = audio_cache_dir()?;
            fs::create_dir_all(&cache_dir)?;
            let key = safe_cache_key(video_id);
            let audio_path = cached_audio_path(&cache_dir, &key, url)?;
            crate::asr::separate_vocals(&audio_path)?
        }
    };
    let samples = decode_audio_mono_f32(&vocals)?;
    build_spectrogram_from_samples(video_id, &samples)
}

fn cached_audio_path(cache_dir: &Path, key: &str, url: &str) -> Result<PathBuf> {
    if let Some(existing) = audio_candidates(cache_dir, key)?.into_iter().next() {
        return Ok(existing);
    }

    let output_template = cache_dir.join(format!("{key}.audio.%(ext)s"));
    let mut cmd = Command::new("yt-dlp");
    cmd.args([
        "--no-playlist",
        "-f",
        "ba[ext=m4a]/ba/b",
        "-o",
        &output_template.to_string_lossy(),
        url,
    ]);
    let output = command_output_with_timeout(cmd, YTDLP_TIMEOUT)
        .context("Could not run yt-dlp to download audio for the timeline spectrogram")?;
    if !output.status.success() {
        return Err(anyhow!(
            "yt-dlp audio download failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    audio_candidates(cache_dir, key)?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("yt-dlp did not produce audio for {url}"))
}

fn audio_candidates(cache_dir: &Path, key: &str) -> Result<Vec<PathBuf>> {
    let prefix = format!("{key}.audio.");
    let mut candidates = fs::read_dir(cache_dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix))
        })
        .collect::<Vec<_>>();
    candidates.sort();
    Ok(candidates)
}

fn decode_audio_mono_f32(audio_path: &Path) -> Result<Vec<f32>> {
    ensure_gstreamer().map_err(|err| anyhow!(err))?;

    let uri = url::Url::from_file_path(audio_path)
        .map_err(|_| anyhow!("Could not build file URI for {}", audio_path.display()))?
        .to_string();
    let pipeline = gst::Pipeline::default();
    let decode = gst::ElementFactory::make("uridecodebin")
        .property("uri", &uri)
        .build()
        .context("Could not create GStreamer uridecodebin")?;
    let convert = gst::ElementFactory::make("audioconvert")
        .build()
        .context("Could not create GStreamer audioconvert")?;
    let resample = gst::ElementFactory::make("audioresample")
        .build()
        .context("Could not create GStreamer audioresample")?;
    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("layout", "interleaved")
        .field("channels", 1i32)
        .field("rate", TARGET_SAMPLE_RATE)
        .build();
    let capsfilter = gst::ElementFactory::make("capsfilter")
        .property("caps", &caps)
        .build()
        .context("Could not create GStreamer capsfilter")?;
    let appsink = gst::ElementFactory::make("appsink")
        .property("caps", &caps)
        .property("emit-signals", false)
        .property("sync", false)
        .build()
        .context("Could not create GStreamer appsink")?
        .dynamic_cast::<gst_app::AppSink>()
        .map_err(|_| anyhow!("GStreamer appsink element had the wrong type"))?;

    pipeline
        .add_many([
            &decode,
            &convert,
            &resample,
            &capsfilter,
            appsink.upcast_ref(),
        ])
        .context("Could not assemble audio decode pipeline")?;
    gst::Element::link_many([&convert, &resample, &capsfilter, appsink.upcast_ref()])
        .context("Could not link audio decode pipeline")?;

    let convert_weak = convert.downgrade();
    decode.connect_pad_added(move |_decode, src_pad| {
        let Some(convert) = convert_weak.upgrade() else {
            return;
        };
        let Some(sink_pad) = convert.static_pad("sink") else {
            return;
        };
        if sink_pad.is_linked() {
            return;
        }
        if !pad_is_audio(src_pad) {
            return;
        }
        let _ = src_pad.link(&sink_pad);
    });

    pipeline
        .set_state(gst::State::Playing)
        .context("Could not start audio decode pipeline")?;
    let result = pull_pcm_samples(&pipeline, &appsink);
    let _ = pipeline.set_state(gst::State::Null);
    result
}

fn pad_is_audio(src_pad: &gst::Pad) -> bool {
    src_pad
        .current_caps()
        .or_else(|| src_pad.query_caps(None).into())
        .and_then(|caps| {
            caps.structure(0)
                .map(|structure| structure.name().starts_with("audio/"))
        })
        .unwrap_or(false)
}

fn pull_pcm_samples(pipeline: &gst::Pipeline, appsink: &gst_app::AppSink) -> Result<Vec<f32>> {
    let bus = pipeline
        .bus()
        .ok_or_else(|| anyhow!("GStreamer audio pipeline has no bus"))?;
    let mut samples = Vec::new();

    loop {
        match appsink.try_pull_sample(gst::ClockTime::from_mseconds(250)) {
            Some(sample) => append_sample_f32(&sample, &mut samples)?,
            None => {
                if drain_bus(&bus)? {
                    break;
                }
            }
        }
    }

    if samples.is_empty() {
        Err(anyhow!("decoded audio produced no PCM samples"))
    } else {
        Ok(samples)
    }
}

fn append_sample_f32(sample: &gst::Sample, out: &mut Vec<f32>) -> Result<()> {
    let buffer = sample
        .buffer()
        .ok_or_else(|| anyhow!("GStreamer sample had no buffer"))?;
    let map = buffer
        .map_readable()
        .context("Could not map GStreamer audio buffer")?;
    out.extend(
        map.as_slice()
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])),
    );
    Ok(())
}

fn drain_bus(bus: &gst::Bus) -> Result<bool> {
    while let Some(message) = bus.pop() {
        use gst::MessageView;
        match message.view() {
            MessageView::Eos(..) => return Ok(true),
            MessageView::Error(err) => {
                return Err(anyhow!(
                    "GStreamer audio decode failed: {}",
                    err.error().message()
                ));
            }
            _ => {}
        }
    }
    Ok(false)
}

fn build_spectrogram_from_samples(video_id: &str, samples: &[f32]) -> Result<AudioSpectrogram> {
    let hop_length = hop_length_for(samples.len());
    let raw = par_compute_spectrogram(
        samples,
        N_FFT,
        hop_length,
        WIN_LENGTH,
        false,
        SpectrogramType::Power,
    );
    let pixels = smooth_spectrogram(&spectrogram_to_intensities(&raw, FREQ_BINS));
    let width = raw.first().map(Vec::len).unwrap_or(0);
    let waveform = waveform_envelope(samples, hop_length, width);
    Ok(AudioSpectrogram {
        video_id: video_id.to_string(),
        width,
        height: FREQ_BINS,
        pixels,
        waveform,
    })
}

fn hop_length_for(sample_count: usize) -> usize {
    if sample_count <= WIN_LENGTH {
        return MIN_HOP_LENGTH;
    }
    ((sample_count - WIN_LENGTH) / TARGET_TIME_BINS).max(MIN_HOP_LENGTH)
}

fn spectrogram_to_intensities(raw: &[Vec<f32>], output_bins: usize) -> Vec<u8> {
    let Some(width) = raw.first().map(Vec::len).filter(|width| *width > 0) else {
        return Vec::new();
    };
    let mut db_values = vec![-120.0f32; width * output_bins];

    for y in 0..output_bins {
        let (start, end) = log_frequency_range(y, output_bins, raw.len());
        for x in 0..width {
            let mut count = 0usize;
            let summed_power = raw[start..end]
                .iter()
                .filter_map(|band| band.get(x).copied())
                .inspect(|_| count += 1)
                .sum::<f32>();
            let average_power = if count == 0 {
                0.0
            } else {
                summed_power / count as f32
            };
            let db = 10.0 * average_power.max(1.0e-12).log10();
            let idx = (output_bins - 1 - y) * width + x;
            db_values[idx] = db;
        }
    }

    let ceiling = percentile(db_values.clone(), 0.995);
    let noise_floor = percentile(db_values.clone(), 0.22);
    let floor = noise_floor.max(ceiling - DISPLAY_DYNAMIC_RANGE_DB);
    let span = (ceiling - floor).max(1.0);

    db_values
        .into_iter()
        .map(|db| {
            let normalized = ((db - floor) / span).clamp(0.0, 1.0);
            let gated = ((normalized - 0.07) / 0.93).clamp(0.0, 1.0);
            (gated.powf(1.35) * 255.0) as u8
        })
        .collect()
}

fn smooth_spectrogram(pixels: &[u8]) -> Vec<u8> {
    if pixels.len() < FREQ_BINS {
        return pixels.to_vec();
    }
    let width = pixels.len() / FREQ_BINS;
    if width == 0 {
        return Vec::new();
    }
    // Light, mostly-vertical smoothing: it knocks down the banding between
    // adjacent log-frequency rows without blurring along time, so transients
    // and vocal detail stay sharp instead of washing into a smooth blob.
    let mut smoothed = vec![0u8; pixels.len()];
    for y in 0..FREQ_BINS {
        for x in 0..width {
            let mut weighted_sum = 0u32;
            let mut weight_sum = 0u32;
            for dy in -1isize..=1 {
                for dx in -1isize..=1 {
                    let Some(sx) = x.checked_add_signed(dx) else {
                        continue;
                    };
                    let Some(sy) = y.checked_add_signed(dy) else {
                        continue;
                    };
                    if sx >= width || sy >= FREQ_BINS {
                        continue;
                    }
                    let weight = match (dx.abs(), dy.abs()) {
                        (0, 0) => 16,
                        (0, 1) => 4,
                        (1, 0) => 2,
                        _ => 1,
                    };
                    weighted_sum += pixels[sy * width + sx] as u32 * weight;
                    weight_sum += weight;
                }
            }
            smoothed[y * width + x] = (weighted_sum / weight_sum.max(1)) as u8;
        }
    }
    smoothed
}

fn waveform_envelope(samples: &[f32], hop_length: usize, width: usize) -> Vec<u8> {
    if samples.is_empty() || width == 0 {
        return Vec::new();
    }
    let mut values = (0..width)
        .map(|frame| {
            let start = frame.saturating_mul(hop_length);
            let end = (start + hop_length).min(samples.len());
            if start >= end {
                return 0.0;
            }
            let slice = &samples[start..end];
            let rms = (slice.iter().map(|sample| sample * sample).sum::<f32>()
                / slice.len() as f32)
                .sqrt();
            let peak = slice
                .iter()
                .map(|sample| sample.abs())
                .fold(0.0f32, f32::max);
            rms * 0.72 + peak * 0.28
        })
        .collect::<Vec<_>>();
    smooth_envelope(&mut values);
    let ceiling = percentile(values.clone(), 0.985).max(1.0e-6);
    values
        .into_iter()
        .map(|value| ((value / ceiling).clamp(0.0, 1.0).powf(0.55) * 255.0) as u8)
        .collect()
}

fn smooth_envelope(values: &mut [f32]) {
    if values.len() < 3 {
        return;
    }
    let original = values.to_vec();
    for idx in 0..values.len() {
        let start = idx.saturating_sub(2);
        let end = (idx + 3).min(original.len());
        values[idx] = original[start..end].iter().sum::<f32>() / (end - start) as f32;
    }
}

fn percentile(mut values: Vec<f32>, quantile: f32) -> f32 {
    values.retain(|value| value.is_finite());
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|left, right| left.total_cmp(right));
    let index = ((values.len() - 1) as f32 * quantile.clamp(0.0, 1.0)).round() as usize;
    values[index]
}

fn log_frequency_range(y: usize, output_bins: usize, input_bins: usize) -> (usize, usize) {
    if input_bins <= 1 {
        return (0, input_bins.max(1));
    }
    let max_index = input_bins - 1;
    let low = log_bin_edge(y, output_bins, max_index);
    let high = log_bin_edge(y + 1, output_bins, max_index).max(low + 1);
    (low.min(max_index), high.min(input_bins))
}

fn log_bin_edge(edge: usize, output_bins: usize, max_index: usize) -> usize {
    if edge == 0 {
        return 0;
    }
    let normalized = edge as f32 / output_bins.max(1) as f32;
    let max = max_index.max(1) as f32;
    (max.powf(normalized).round() as usize).clamp(1, max_index.max(1))
}

fn audio_cache_dir() -> Result<PathBuf> {
    dirs::cache_dir()
        .map(|path| path.join("kpopmvlyrics").join("timeline-audio"))
        .ok_or_else(|| anyhow!("Could not resolve cache directory"))
}

fn safe_cache_key(value: &str) -> String {
    let cleaned = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(80)
        .collect::<String>();
    if cleaned.is_empty() {
        "video".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_spectrogram_from_samples, hop_length_for, percentile, spectrogram_to_intensities,
        waveform_envelope,
    };

    #[test]
    fn chooses_reasonable_hop_for_short_audio() {
        assert_eq!(hop_length_for(100), 96);
    }

    #[test]
    fn maps_native_spectrogram_matrix_to_pixels() {
        let raw = vec![vec![1.0, 10.0], vec![100.0, 1_000.0], vec![0.01, 0.1]];
        let pixels = spectrogram_to_intensities(&raw, 2);

        assert_eq!(pixels.len(), 4);
        assert!(pixels.iter().any(|value| *value > 0));
    }

    #[test]
    fn percentile_scaling_keeps_noise_floor_dark() {
        let mut raw = vec![vec![1.0; 96]; 16];
        raw[4][20] = 100_000.0;
        raw[5][20] = 120_000.0;
        let pixels = spectrogram_to_intensities(&raw, 8);
        let bright = pixels.iter().filter(|value| **value > 220).count();

        assert!(bright < pixels.len() / 8);
    }

    #[test]
    fn computes_percentiles() {
        assert_eq!(percentile(vec![1.0, 3.0, 2.0], 0.5), 2.0);
    }

    #[test]
    fn builds_waveform_envelope() {
        let samples = (0..2_048)
            .map(|idx| if idx < 1_024 { 0.05 } else { 0.5 })
            .collect::<Vec<_>>();
        let envelope = waveform_envelope(&samples, 256, 8);

        assert_eq!(envelope.len(), 8);
        assert!(envelope[7] > envelope[0]);
    }

    #[test]
    fn builds_spectrogram_from_pcm_samples() {
        let samples = (0..44_100)
            .map(|idx| ((idx as f32 / 22_050.0) * 440.0 * std::f32::consts::TAU).sin())
            .collect::<Vec<_>>();

        let spectrogram = build_spectrogram_from_samples("test", &samples).unwrap();

        assert_eq!(spectrogram.video_id, "test");
        assert_eq!(spectrogram.height, 128);
        assert_eq!(
            spectrogram.pixels.len(),
            spectrogram.width * spectrogram.height
        );
        assert_eq!(spectrogram.waveform.len(), spectrogram.width);
    }
}
