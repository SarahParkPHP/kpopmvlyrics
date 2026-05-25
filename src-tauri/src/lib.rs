mod align;
mod captions;
mod db;
mod lyrics;
mod media_server;
mod members;
mod models;
mod video;

use std::path::PathBuf;
use std::sync::Mutex;

use align::{align_lines, AlignmentInput};
use captions::{parse_caption_text, CaptionProvider, YouTubeCaptionProvider};
use db::Repository;
use lyrics::{ColorCodedLyricsProvider, GeniusProvider, LyricsProvider};
use media_server::MediaServer;
use members::{KpopFandomProvider, KpoppingProvider, MemberProfileProvider};
use models::*;
use tauri::Manager;
use video::{cleanup_incomplete_downloads, list_video_formats_inner, resolve_video_metadata_inner, resolve_video_stream_inner};

struct AppState {
    repo: Mutex<Repository>,
    video_cache_dir: PathBuf,
    media_server: MediaServer,
}

#[tauri::command]
fn resolve_video_metadata(url: String) -> Result<VideoMetadata, String> {
    resolve_video_metadata_inner(&url).map_err(to_string)
}

#[tauri::command]
fn list_video_formats(url: String) -> Result<Vec<VideoFormat>, String> {
    list_video_formats_inner(&url).map_err(to_string)
}

#[tauri::command]
async fn resolve_video_stream(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    url: String,
    format_id: Option<String>,
) -> Result<String, String> {
    let cache_dir = state.video_cache_dir.clone();
    let media_server = state.media_server.clone();
    tauri::async_runtime::spawn_blocking(move || {
        resolve_video_stream_inner(
            &url,
            format_id.as_deref(),
            &cache_dir,
            &media_server,
            Some(app),
        )
    })
    .await
    .map_err(|err| err.to_string())?
    .map_err(to_string)
}

#[tauri::command]
fn fetch_lyrics(query: String, state: tauri::State<'_, AppState>) -> Result<SongPackage, String> {
    let providers: Vec<Box<dyn LyricsProvider>> = vec![
        Box::new(ColorCodedLyricsProvider::default()),
        Box::new(GeniusProvider::default()),
    ];

    let mut last_error = None;
    for provider in providers {
        match provider.fetch(&query) {
            Ok(mut package) => {
                let mut repo = state.repo.lock().map_err(to_string)?;
                repo.upsert_song_package(&mut package).map_err(to_string)?;
                return Ok(package);
            }
            Err(err) => last_error = Some(err.to_string()),
        }
    }

    Err(last_error.unwrap_or_else(|| "No lyric providers configured".to_string()))
}

#[tauri::command]
fn import_lyrics(
    raw_text: String,
    title: String,
    artist: String,
    state: tauri::State<'_, AppState>,
) -> Result<SongPackage, String> {
    let mut package = lyrics::parse_manual_lyrics(&raw_text, &title, &artist).map_err(to_string)?;
    let mut repo = state.repo.lock().map_err(to_string)?;
    repo.upsert_song_package(&mut package).map_err(to_string)?;
    Ok(package)
}

#[tauri::command]
fn fetch_captions(
    video_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<CaptionLine>, String> {
    let provider = YouTubeCaptionProvider::default();
    let captions = provider.fetch(&video_id).map_err(to_string)?;
    let mut repo = state.repo.lock().map_err(to_string)?;
    repo.upsert_caption_lines(&video_id, &captions)
        .map_err(to_string)?;
    Ok(captions)
}

#[tauri::command]
fn import_captions(
    video_id: String,
    raw_text: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<CaptionLine>, String> {
    let captions = parse_caption_text(&raw_text).map_err(to_string)?;
    let mut repo = state.repo.lock().map_err(to_string)?;
    repo.upsert_caption_lines(&video_id, &captions)
        .map_err(to_string)?;
    Ok(captions)
}

#[tauri::command]
fn align_lyrics(
    song_id: i64,
    video_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<AlignmentLine>, String> {
    let mut repo = state.repo.lock().map_err(to_string)?;
    let lyrics = repo.lyric_lines(song_id).map_err(to_string)?;
    let captions = repo.caption_lines(&video_id).map_err(to_string)?;
    let aligned = align_lines(AlignmentInput { lyrics, captions });
    repo.upsert_alignment(song_id, &video_id, &aligned)
        .map_err(to_string)?;
    Ok(aligned)
}

#[tauri::command]
fn save_alignment_edits(
    song_id: i64,
    video_id: String,
    lines: Vec<AlignmentLine>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let mut repo = state.repo.lock().map_err(to_string)?;
    repo.upsert_alignment(song_id, &video_id, &lines)
        .map_err(to_string)
}

#[tauri::command]
fn search_member_profiles(
    group_name: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<MemberProfile>, String> {
    let providers: Vec<Box<dyn MemberProfileProvider>> = vec![
        Box::new(KpoppingProvider::default()),
        Box::new(KpopFandomProvider::default()),
    ];
    let mut profiles = Vec::new();
    for provider in providers {
        if let Ok(mut found) = provider.search(&group_name) {
            profiles.append(&mut found);
        }
    }
    profiles.sort_by(|a, b| a.stage_name.cmp(&b.stage_name));
    profiles.dedup_by(|a, b| a.stage_name.eq_ignore_ascii_case(&b.stage_name));

    let mut repo = state.repo.lock().map_err(to_string)?;
    repo.upsert_members(&group_name, &profiles)
        .map_err(to_string)?;
    Ok(profiles)
}

#[tauri::command]
fn save_member_override(
    group_name: String,
    member: MemberProfile,
    state: tauri::State<'_, AppState>,
) -> Result<MemberProfile, String> {
    let mut repo = state.repo.lock().map_err(to_string)?;
    repo.save_member_override(&group_name, &member)
        .map_err(to_string)?;
    Ok(member)
}

pub fn run() {
    configure_linux_webview_backend();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let app_dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::env::current_dir().expect("current dir"));
            std::fs::create_dir_all(&app_dir)?;
            let video_cache_dir = app_dir.join("video-cache");
            std::fs::create_dir_all(&video_cache_dir)?;
            cleanup_incomplete_downloads(&video_cache_dir).map_err(to_string)?;
            app.asset_protocol_scope()
                .allow_directory(&video_cache_dir, true)
                .map_err(to_string)?;
            let media_server = MediaServer::start(video_cache_dir.clone())?;
            let repo = Repository::open(app_dir.join("kpopmvlyrics.sqlite3"))?;
            app.manage(AppState {
                repo: Mutex::new(repo),
                video_cache_dir,
                media_server,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            resolve_video_metadata,
            list_video_formats,
            resolve_video_stream,
            fetch_lyrics,
            import_lyrics,
            fetch_captions,
            import_captions,
            align_lyrics,
            save_alignment_edits,
            search_member_profiles,
            save_member_override
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn to_string<E: std::fmt::Display>(err: E) -> String {
    err.to_string()
}

#[cfg(target_os = "linux")]
fn configure_linux_webview_backend() {
    if std::env::var_os("KPOPMVLYRICS_HW_ACCEL").is_some() {
        return;
    }

    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    if std::env::var_os("WEBKIT_GST_ALLOWED_URI_PROTOCOLS").is_none() {
        std::env::set_var("WEBKIT_GST_ALLOWED_URI_PROTOCOLS", "asset,file,http,https");
    }
}

#[cfg(not(target_os = "linux"))]
fn configure_linux_webview_backend() {}
