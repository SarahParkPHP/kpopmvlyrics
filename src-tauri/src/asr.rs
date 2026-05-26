use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::log::{verbose, PhaseGuard};
use crate::models::CaptionLine;
use crate::process_util::{command_output_with_timeout, WHISPER_TIMEOUT, YTDLP_TIMEOUT};

const ASR_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub const SMALL_ASR_MODEL: &str = "Qwen/Qwen3-ASR-0.6B";
pub const LARGE_ASR_MODEL: &str = "Qwen/Qwen3-ASR-1.7B";
pub const ALIGNER_MODEL: &str = "Qwen/Qwen3-ForcedAligner-0.6B";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AsrModelSize {
    #[default]
    Small,
    Large,
}

impl AsrModelSize {
    pub fn hf_model_id(self) -> &'static str {
        match self {
            Self::Small => SMALL_ASR_MODEL,
            Self::Large => LARGE_ASR_MODEL,
        }
    }

    pub fn model_filename(self) -> &'static str {
        self.hf_model_id()
    }

    pub fn backend(self) -> &'static str {
        "qwen-asr"
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Small => "Qwen3-ASR 0.6B (faster)",
            Self::Large => "Qwen3-ASR 1.7B (more accurate)",
        }
    }

    pub fn from_storage(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "large" | "large-v3" | "1.7b" | "qwen3-1.7b" | "qwen3-asr-1.7b" => Self::Large,
            _ => Self::Small,
        }
    }

    pub fn as_storage(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Large => "large",
        }
    }
}

pub fn effective_asr_model(configured: AsrModelSize) -> AsrModelSize {
    if let Ok(value) = std::env::var("KPOPMVLYRICS_ASR_MODEL") {
        return AsrModelSize::from_storage(&value);
    }
    configured
}

pub fn effective_asr_device() -> String {
    match std::env::var("KPOPMVLYRICS_ASR_DEVICE") {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "cpu" => "cpu".into(),
            "cuda" | "gpu" => "cuda".into(),
            _ => "auto".into(),
        },
        Err(_) => "auto".into(),
    }
}

pub fn forced_align_language(use_original: bool, language_hint: Option<&str>) -> &'static str {
    if use_original {
        return match language_hint.map(str::trim) {
            Some("ko") => "Korean",
            Some("ja") => "Japanese",
            Some("zh") | Some("zh-cn") => "Chinese",
            Some("yue") => "Cantonese",
            Some("fr") => "French",
            Some("de") => "German",
            Some("it") => "Italian",
            Some("pt") => "Portuguese",
            Some("ru") => "Russian",
            Some("es") => "Spanish",
            _ => "Korean",
        };
    }
    "English"
}

#[derive(Debug, Clone, Deserialize)]
pub struct AsrWord {
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AsrLineTiming {
    pub lyric_index: usize,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ForcedAlignLine {
    pub index: usize,
    pub text: String,
    pub char_start: usize,
    pub char_end: usize,
}

#[derive(Debug, Clone, Deserialize)]
struct AsrSegment {
    text: String,
    start_ms: i64,
    end_ms: i64,
    #[serde(default)]
    words: Vec<AsrWord>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AsrTranscript {
    pub language: Option<String>,
    pub model: Option<String>,
    pub backend: Option<String>,
    pub device: Option<String>,
    pub words: Vec<AsrWord>,
    #[serde(default)]
    pub segments: Vec<AsrSegment>,
    #[serde(default)]
    pub alignment_source: Option<String>,
    #[serde(default)]
    pub line_timings: Vec<AsrLineTiming>,
}

fn scrub_appimage_python_env(command: &mut Command) {
    for key in [
        "APPDIR",
        "APPIMAGE",
        "ARGV0",
        "REDIRECT_APPIMAGE",
        "TARGET_APPIMAGE",
        "DESKTOPINTEGRATION",
    ] {
        command.env_remove(key);
    }
    if let Ok(path) = std::env::var("PATH") {
        let cleaned = path
            .split(':')
            .filter(|entry| {
                !entry.contains(".mount_cursor")
                    && !entry.contains("AppImages/cursor")
                    && !entry.contains("/cursor/resources/")
            })
            .collect::<Vec<_>>()
            .join(":");
        if !cleaned.is_empty() {
            command.env("PATH", cleaned);
        }
    }
}

pub fn asr_available() -> bool {
    let _phase = PhaseGuard::begin("asr_available probe");
    let python = match asr_python() {
        Ok(path) => path,
        Err(err) => {
            verbose(format!("asr python not found: {err}"));
            return false;
        }
    };
    verbose(format!("asr probe python={}", python.display()));
    let mut command = Command::new(&python);
    scrub_appimage_python_env(&mut command);
    command.args(["-c", "from qwen_asr import Qwen3ASRModel, Qwen3ForcedAligner; import torch"]);
    let available = command_output_with_timeout(command, ASR_PROBE_TIMEOUT)
        .map(|output| output.status.success())
        .unwrap_or(false);
    verbose(format!("asr_available={available}"));
    available
}

pub fn asr_setup_hint() -> &'static str {
    "Run ./scripts/setup-asr.sh from the project root (installs official qwen-asr for PyTorch)."
}

pub fn transcribe_video(
    video_id: &str,
    language_hint: Option<&str>,
    model_size: AsrModelSize,
    lyrics_text: Option<&str>,
    forced_lines: Option<&[ForcedAlignLine]>,
    align_language: Option<&str>,
) -> Result<AsrTranscript> {
    verbose(format!("asr transcribe_video video_id={video_id}"));
    let cache_dir = asr_cache_dir()?;
    std::fs::create_dir_all(&cache_dir).context("create asr cache dir")?;
    let audio_path = {
        let _phase = PhaseGuard::begin("asr download_audio");
        download_audio(video_id, &cache_dir)?
    };
    verbose(format!("asr audio={}", audio_path.display()));
    transcribe_audio(
        &audio_path,
        language_hint,
        model_size,
        lyrics_text,
        forced_lines,
        align_language,
    )
}

pub fn transcribe_audio(
    audio_path: &Path,
    language_hint: Option<&str>,
    model_size: AsrModelSize,
    lyrics_text: Option<&str>,
    forced_lines: Option<&[ForcedAlignLine]>,
    align_language: Option<&str>,
) -> Result<AsrTranscript> {
    let script = asr_script_path()?;
    if !script.exists() {
        return Err(anyhow!("ASR script not found at {}", script.display()));
    }
    if !asr_available() {
        return Err(anyhow!(
            "Qwen3 ASR (qwen-asr) is not installed. {}",
            asr_setup_hint()
        ));
    }

    let python = asr_python()?;

    let output_path = std::env::temp_dir().join(format!(
        "kpopmvlyrics-asr-{}.json",
        std::process::id()
    ));
    let lyrics_lines_path = std::env::temp_dir().join(format!(
        "kpopmvlyrics-asr-lines-{}.json",
        std::process::id()
    ));
    let _guard = TempFileGuard(output_path.clone());
    let _lines_guard = forced_lines
        .filter(|lines| !lines.is_empty())
        .map(|_| TempFileGuard(lyrics_lines_path.clone()));

    verbose(format!(
        "asr model={} aligner={} device={} align_language={:?}",
        model_size.hf_model_id(),
        ALIGNER_MODEL,
        effective_asr_device(),
        align_language
    ));

    let mut command = Command::new(&python);
    scrub_appimage_python_env(&mut command);
    command
        .arg(&script)
        .arg("--audio")
        .arg(audio_path)
        .arg("--output")
        .arg(&output_path)
        .arg("--model")
        .arg(model_size.hf_model_id())
        .arg("--aligner-model")
        .arg(ALIGNER_MODEL)
        .arg("--device")
        .arg(effective_asr_device());
    if let Some(language) = language_hint.filter(|value| !value.is_empty()) {
        command.arg("--language").arg(language);
    }
    if let Some(language) = align_language.filter(|value| !value.is_empty()) {
        command.arg("--align-language").arg(language);
    }
    if let Some(lyrics) = lyrics_text.filter(|value| !value.trim().is_empty()) {
        command.arg("--lyrics-text").arg(lyrics);
    }
    if let Some(lines) = forced_lines.filter(|lines| !lines.is_empty()) {
        let body = serde_json::to_string(lines).context("serialize forced align lines")?;
        std::fs::write(&lyrics_lines_path, body)
            .with_context(|| format!("write {}", lyrics_lines_path.display()))?;
        command
            .arg("--lyrics-lines-file")
            .arg(&lyrics_lines_path);
    }

    let output = command_output_with_timeout(command, WHISPER_TIMEOUT)
        .map_err(|err| anyhow!("Could not run Qwen3 ASR script: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(anyhow!(
            "Qwen3 ASR failed: {}{}",
            stderr.trim(),
            if stdout.trim().is_empty() {
                String::new()
            } else {
                format!("\n{}", stdout.trim())
            }
        ));
    }

    let body = std::fs::read_to_string(&output_path)
        .with_context(|| format!("read asr output {}", output_path.display()))?;
    serde_json::from_str(&body).context("parse asr JSON output")
}

pub fn asr_caption_lines(video_id: &str, transcript: &AsrTranscript) -> Vec<CaptionLine> {
    if !transcript.segments.is_empty() {
        return transcript
            .segments
            .iter()
            .enumerate()
            .filter(|(_, segment)| !segment.text.trim().is_empty())
            .map(|(index, segment)| CaptionLine {
                id: None,
                video_id: video_id.to_string(),
                index,
                start_ms: segment.start_ms,
                end_ms: segment.end_ms.max(segment.start_ms + 200),
                text: segment.text.trim().to_string(),
            })
            .collect();
    }

    transcript
        .words
        .chunks(12)
        .enumerate()
        .map(|(index, chunk)| {
            let start_ms = chunk.first().map(|word| word.start_ms).unwrap_or(0);
            let end_ms = chunk
                .last()
                .map(|word| word.end_ms.max(word.start_ms + 200))
                .unwrap_or(start_ms + 900);
            CaptionLine {
                id: None,
                video_id: video_id.to_string(),
                index,
                start_ms,
                end_ms,
                text: chunk
                    .iter()
                    .map(|word| word.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            }
        })
        .filter(|line| !line.text.trim().is_empty())
        .collect()
}

fn download_audio(video_id: &str, cache_dir: &Path) -> Result<PathBuf> {
    let wav_path = cache_dir.join(format!("{video_id}.wav"));
    if wav_path.exists() {
        return Ok(wav_path);
    }

    let url = format!("https://www.youtube.com/watch?v={video_id}");
    let output_template = cache_dir.join(format!("{video_id}.%(ext)s"));
    let mut cmd = Command::new("yt-dlp");
    cmd.args([
        "--no-playlist",
        "-f",
        "ba[ext=m4a]/ba/b",
        "-x",
        "--audio-format",
        "wav",
        "--audio-quality",
        "0",
        "-o",
        &output_template.to_string_lossy(),
        &url,
    ]);
    let output = command_output_with_timeout(cmd, YTDLP_TIMEOUT)
        .context("Could not run yt-dlp to download audio for ASR")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("yt-dlp audio download failed: {}", stderr.trim()));
    }
    if wav_path.exists() {
        return Ok(wav_path);
    }

    let mut candidates = std::fs::read_dir(cache_dir)
        .context("read asr cache dir")?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| stem == video_id)
        })
        .collect::<Vec<_>>();
    candidates.sort();
    candidates
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("yt-dlp did not produce audio for {video_id}"))
}

fn asr_cache_dir() -> Result<PathBuf> {
    dirs::cache_dir()
        .map(|path| path.join("kpopmvlyrics").join("whisper-audio"))
        .ok_or_else(|| anyhow!("Could not resolve cache directory"))
}

fn asr_script_path() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("KPOPMVLYRICS_ASR_SCRIPT") {
        return Ok(PathBuf::from(path));
    }
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts/run_qwen_asr.py"))
}

fn asr_python() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("KPOPMVLYRICS_ASR_PYTHON") {
        return Ok(PathBuf::from(path));
    }

    if let Some(path) = find_project_venv_python() {
        return Ok(path);
    }

    Ok(PathBuf::from("/usr/bin/python3"))
}

fn find_project_venv_python() -> Option<PathBuf> {
    let compile_time = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.venv-asr/bin/python");
    if compile_time.is_file() {
        return Some(compile_time);
    }

    let mut dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    for _ in 0..8 {
        let candidate = dir.join(".venv-asr/bin/python");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

struct TempFileGuard(PathBuf);

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asr_model_size_round_trips_storage() {
        assert_eq!(AsrModelSize::Small.as_storage(), "small");
        assert_eq!(AsrModelSize::Large.as_storage(), "large");
        assert_eq!(AsrModelSize::from_storage("large"), AsrModelSize::Large);
        assert_eq!(AsrModelSize::from_storage("small"), AsrModelSize::Small);
        assert_eq!(AsrModelSize::Small.hf_model_id(), SMALL_ASR_MODEL);
    }

    #[test]
    fn effective_asr_device_reads_env() {
        std::env::set_var("KPOPMVLYRICS_ASR_DEVICE", "cuda");
        assert_eq!(effective_asr_device(), "cuda");
        std::env::set_var("KPOPMVLYRICS_ASR_DEVICE", "cpu");
        assert_eq!(effective_asr_device(), "cpu");
        std::env::remove_var("KPOPMVLYRICS_ASR_DEVICE");
        assert_eq!(effective_asr_device(), "auto");
    }

    #[test]
    fn forced_align_language_maps_codes() {
        assert_eq!(forced_align_language(true, Some("ko")), "Korean");
        assert_eq!(forced_align_language(false, Some("ko")), "English");
        assert_eq!(forced_align_language(true, Some("ja")), "Japanese");
    }

    #[test]
    fn parses_asr_json_fixture() {
        let body = r##"{
            "language": "en",
            "model": "Qwen/Qwen3-ASR-0.6B",
            "backend": "qwen-asr",
            "device": "cuda",
            "alignment_source": "lyrics",
            "words": [
                {"text": "This", "start_ms": 1000, "end_ms": 1200},
                {"text": "is", "start_ms": 1200, "end_ms": 1350}
            ],
            "segments": [
                {
                    "text": "This is",
                    "start_ms": 1000,
                    "end_ms": 1350,
                    "words": []
                }
            ]
        }"##;
        let transcript: AsrTranscript = serde_json::from_str(body).unwrap();
        assert_eq!(transcript.words.len(), 2);
        let captions = asr_caption_lines("video", &transcript);
        assert_eq!(captions.len(), 1);
        assert_eq!(captions[0].text, "This is");
    }
}
