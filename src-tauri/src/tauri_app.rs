#[cfg(not(target_os = "linux"))]
use tauri::Manager;

#[cfg(not(target_os = "linux"))]
use crate::app::AppContext;
#[cfg(not(target_os = "linux"))]
use crate::models::*;
#[cfg(not(target_os = "linux"))]
use crate::player::{
    defer_window_setup, player_load, player_pause, player_play, player_seek, player_set_quality,
    resolve_stream, PlayerState,
};

#[cfg(not(target_os = "linux"))]
struct TauriAppState {
    ctx: AppContext,
}

#[cfg(not(target_os = "linux"))]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let ctx = AppContext::open()?;
            app.manage(TauriAppState { ctx });
            app.manage(PlayerState::new(app.handle().clone()));
            defer_window_setup(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            resolve_video_metadata,
            list_video_formats,
            resolve_stream,
            player_load,
            player_play,
            player_pause,
            player_seek,
            player_set_quality,
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

#[cfg(target_os = "linux")]
pub fn run() {
    crate::ui::run();
}

#[cfg(not(target_os = "linux"))]
#[tauri::command]
fn resolve_video_metadata(
    url: String,
    state: tauri::State<'_, TauriAppState>,
) -> Result<VideoMetadata, String> {
    state.ctx.resolve_video_metadata(&url)
}

#[cfg(not(target_os = "linux"))]
#[tauri::command]
fn list_video_formats(
    url: String,
    state: tauri::State<'_, TauriAppState>,
) -> Result<Vec<VideoFormat>, String> {
    state.ctx.list_video_formats(&url)
}

#[cfg(not(target_os = "linux"))]
#[tauri::command]
fn fetch_lyrics(query: String, state: tauri::State<'_, TauriAppState>) -> Result<SongPackage, String> {
    state.ctx.fetch_lyrics(&query)
}

#[cfg(not(target_os = "linux"))]
#[tauri::command]
fn import_lyrics(
    raw_text: String,
    title: String,
    artist: String,
    state: tauri::State<'_, TauriAppState>,
) -> Result<SongPackage, String> {
    state.ctx.import_lyrics(&raw_text, &title, &artist)
}

#[cfg(not(target_os = "linux"))]
#[tauri::command]
fn fetch_captions(
    video_id: String,
    state: tauri::State<'_, TauriAppState>,
) -> Result<Vec<CaptionLine>, String> {
    state.ctx.fetch_captions(&video_id)
}

#[cfg(not(target_os = "linux"))]
#[tauri::command]
fn import_captions(
    video_id: String,
    raw_text: String,
    state: tauri::State<'_, TauriAppState>,
) -> Result<Vec<CaptionLine>, String> {
    state.ctx.import_captions(&video_id, &raw_text)
}

#[cfg(not(target_os = "linux"))]
#[tauri::command]
fn align_lyrics(
    song_id: i64,
    video_id: String,
    state: tauri::State<'_, TauriAppState>,
) -> Result<Vec<AlignmentLine>, String> {
    state.ctx.align_lyrics(song_id, &video_id)
}

#[cfg(not(target_os = "linux"))]
#[tauri::command]
fn save_alignment_edits(
    song_id: i64,
    video_id: String,
    lines: Vec<AlignmentLine>,
    state: tauri::State<'_, TauriAppState>,
) -> Result<(), String> {
    state.ctx.save_alignment_edits(song_id, &video_id, &lines)
}

#[cfg(not(target_os = "linux"))]
#[tauri::command]
fn search_member_profiles(
    group_name: String,
    state: tauri::State<'_, TauriAppState>,
) -> Result<Vec<MemberProfile>, String> {
    state.ctx.search_member_profiles(&group_name)
}

#[cfg(not(target_os = "linux"))]
#[tauri::command]
fn save_member_override(
    group_name: String,
    member: MemberProfile,
    state: tauri::State<'_, TauriAppState>,
) -> Result<MemberProfile, String> {
    state.ctx.save_member_override(&group_name, &member)
}
