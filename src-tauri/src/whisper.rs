use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::models::CaptionLine;

#[derive(Debug, Clone, Deserialize)]
pub struct WhisperWord {
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WhisperSegment {
    text: String,
    start_ms: i64,
    end_ms: i64,
    #[serde(default)]
    words: Vec<WhisperWord>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WhisperTranscript {
    pub language: Option<String>,
    pub language_probability: Option<f64>,
    pub model: Option<String>,
    pub device: Option<String>,
    pub words: Vec<WhisperWord>,
    #[serde(default)]
    pub segments: Vec<WhisperSegment>,
}

pub fn whisper_available() -> bool {
    whisper_python()
        .ok()
        .is_some_and(|python| {
            Command::new(&python)
                .args(["-c", "import faster_whisper"])
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        })
}

pub fn whisper_setup_hint() -> &'static str {
    "Run ./scripts/setup-whisper.sh from the project root (creates .venv-whisper)."
}

pub fn transcribe_video(
    video_id: &str,
    language_hint: Option<&str>,
) -> Result<WhisperTranscript> {
    let cache_dir = whisper_cache_dir()?;
    std::fs::create_dir_all(&cache_dir).context("create whisper cache dir")?;
    let audio_path = download_audio(video_id, &cache_dir)?;
    transcribe_audio(&audio_path, language_hint)
}

pub fn transcribe_audio(
    audio_path: &Path,
    language_hint: Option<&str>,
) -> Result<WhisperTranscript> {
    let script = whisper_script_path()?;
    if !script.exists() {
        return Err(anyhow!(
            "Whisper script not found at {}",
            script.display()
        ));
    }
    if !whisper_available() {
        return Err(anyhow!(
            "faster-whisper is not installed. {}",
            whisper_setup_hint()
        ));
    }

    let python = whisper_python()?;

    let output_path = std::env::temp_dir().join(format!(
        "kpopmvlyrics-whisper-{}.json",
        std::process::id()
    ));
    let _guard = TempFileGuard(output_path.clone());

    let model = std::env::var("KPOPMVLYRICS_WHISPER_MODEL").unwrap_or_else(|_| "small".into());
    let device = std::env::var("KPOPMVLYRICS_WHISPER_DEVICE").unwrap_or_else(|_| "auto".into());
    let mut command = Command::new(&python);
    command
        .arg(&script)
        .arg("--audio")
        .arg(audio_path)
        .arg("--output")
        .arg(&output_path)
        .arg("--model")
        .arg(model)
        .arg("--device")
        .arg(device);
    if let Some(language) = language_hint.filter(|value| !value.is_empty()) {
        command.arg("--language").arg(language);
    }

    let output = command
        .output()
        .context("Could not run faster-whisper transcription script")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(anyhow!(
            "Whisper transcription failed: {}{}",
            stderr.trim(),
            if stdout.trim().is_empty() {
                String::new()
            } else {
                format!("\n{}", stdout.trim())
            }
        ));
    }

    let body = std::fs::read_to_string(&output_path)
        .with_context(|| format!("read whisper output {}", output_path.display()))?;
    serde_json::from_str(&body).context("parse whisper JSON output")
}

pub fn whisper_caption_lines(video_id: &str, transcript: &WhisperTranscript) -> Vec<CaptionLine> {
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
    let output = Command::new("yt-dlp")
        .args([
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
        ])
        .output()
        .context("Could not run yt-dlp to download audio for whisper")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("yt-dlp audio download failed: {}", stderr.trim()));
    }
    if wav_path.exists() {
        return Ok(wav_path);
    }

    let mut candidates = std::fs::read_dir(cache_dir)
        .context("read whisper cache dir")?
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

fn whisper_cache_dir() -> Result<PathBuf> {
    dirs::cache_dir()
        .map(|path| path.join("kpopmvlyrics").join("whisper-audio"))
        .ok_or_else(|| anyhow!("Could not resolve cache directory"))
}

fn whisper_script_path() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("KPOPMVLYRICS_WHISPER_SCRIPT") {
        return Ok(PathBuf::from(path));
    }
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts/whisper_transcribe.py"))
}

fn whisper_python() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("KPOPMVLYRICS_WHISPER_PYTHON") {
        return Ok(PathBuf::from(path));
    }

    if let Some(path) = find_project_venv_python() {
        return Ok(path);
    }

    Ok(PathBuf::from("python3"))
}

fn find_project_venv_python() -> Option<PathBuf> {
    let compile_time =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.venv-whisper/bin/python");
    if compile_time.is_file() {
        return Some(compile_time);
    }

    let mut dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    for _ in 0..8 {
        let candidate = dir.join(".venv-whisper/bin/python");
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
    fn parses_whisper_json_fixture() {
        let body = r##"{
            "language": "en",
            "language_probability": 0.99,
            "model": "small",
            "device": "cpu",
            "words": [
                {"text": "This", "start_ms": 1000, "end_ms": 1200},
                {"text": "is", "start_ms": 1200, "end_ms": 1350}
            ],
            "segments": [
                {
                    "text": "This is",
                    "start_ms": 1000,
                    "end_ms": 1350,
                    "words": [
                        {"text": "This", "start_ms": 1000, "end_ms": 1200},
                        {"text": "is", "start_ms": 1200, "end_ms": 1350}
                    ]
                }
            ]
        }"##;
        let transcript: WhisperTranscript = serde_json::from_str(body).unwrap();
        assert_eq!(transcript.words.len(), 2);
        let captions = whisper_caption_lines("video", &transcript);
        assert_eq!(captions.len(), 1);
        assert_eq!(captions[0].text, "This is");
    }
}
