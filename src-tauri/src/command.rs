//! Shared JSON command dispatcher used by every native frontend.
//!
//! Frontends (macOS/UniFFI, Qt/QML, WinUI 3) call [`invoke`] with a command name
//! and a JSON object of camelCase arguments; the result is the command's return
//! value as JSON. This is the single place command routing lives, so every
//! frontend behaves identically.

use serde::Serialize;
use serde_json::Value;

use crate::app::AppContext;
use crate::models::{AlignmentLine, MemberProfile, SongPackage, VideoMetadata};

pub fn invoke(ctx: &AppContext, command: &str, args_json: &str) -> Result<String, String> {
    let trimmed = args_json.trim();
    let args: Value = serde_json::from_str(if trimmed.is_empty() { "{}" } else { trimmed })
        .map_err(|err| format!("invalid args JSON: {err}"))?;

    let str_arg = |key: &str| -> Result<String, String> {
        args.get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("missing string argument '{key}'"))
    };
    let i64_arg = |key: &str| -> Result<i64, String> {
        args.get(key)
            .and_then(Value::as_i64)
            .ok_or_else(|| format!("missing integer argument '{key}'"))
    };

    match command {
        "resolve_video_metadata" => to_json(ctx.resolve_video_metadata(&str_arg("url")?)?),
        "list_video_formats" => to_json(ctx.list_video_formats(&str_arg("url")?)?),
        "resolve_stream" => {
            let url = str_arg("url")?;
            let format_id = args.get("formatId").and_then(Value::as_str);
            to_json(ctx.resolve_stream(&url, format_id)?)
        }
        "fetch_lyrics" => to_json(ctx.fetch_lyrics(&str_arg("query")?)?),
        "import_lyrics" => to_json(ctx.import_lyrics(
            &str_arg("rawText")?,
            &str_arg("title")?,
            &str_arg("artist")?,
        )?),
        "fetch_captions" => to_json(ctx.fetch_captions(&str_arg("videoId")?)?),
        "import_captions" => {
            to_json(ctx.import_captions(&str_arg("videoId")?, &str_arg("rawText")?)?)
        }
        "align_lyrics" => {
            let result = ctx.align_lyrics(i64_arg("songId")?, &str_arg("videoId")?)?;
            to_json(result.alignment)
        }
        "save_alignment_edits" => {
            let lines: Vec<AlignmentLine> = from_arg(&args, "lines")?;
            ctx.save_alignment_edits(i64_arg("songId")?, &str_arg("videoId")?, &lines)?;
            Ok("null".to_string())
        }
        "search_member_profiles" => {
            to_json(ctx.search_member_profiles(&str_arg("groupName")?)?)
        }
        "save_member_override" => {
            let member: MemberProfile = from_arg(&args, "member")?;
            to_json(ctx.save_member_override(&str_arg("groupName")?, &member)?)
        }
        // Pure transform (no AppContext use): the canonical export shape.
        "build_export" => {
            let metadata: VideoMetadata = from_arg(&args, "metadata")?;
            let song: SongPackage = from_arg(&args, "songPackage")?;
            let alignment: Vec<AlignmentLine> = from_arg(&args, "alignment")?;
            to_json(crate::export::build_export_json(&metadata, &song, &alignment))
        }
        other => Err(format!("unknown command '{other}'")),
    }
}

fn to_json<T: Serialize>(value: T) -> Result<String, String> {
    serde_json::to_string(&value).map_err(|err| format!("serialize failed: {err}"))
}

fn from_arg<T: serde::de::DeserializeOwned>(args: &Value, key: &str) -> Result<T, String> {
    let value = args
        .get(key)
        .ok_or_else(|| format!("missing argument '{key}'"))?;
    serde_json::from_value(value.clone()).map_err(|err| format!("invalid argument '{key}': {err}"))
}

#[cfg(test)]
mod tests {
    use super::{from_arg, to_json};
    use crate::models::AlignmentLine;
    use serde_json::json;

    #[test]
    fn to_json_serializes_value() {
        assert_eq!(to_json(vec![1, 2, 3]).unwrap(), "[1,2,3]");
    }

    #[test]
    fn from_arg_decodes_camelcase_records() {
        let args = json!({
            "lines": [{
                "lyricIndex": 0,
                "captionIndex": null,
                "startMs": 1000,
                "endMs": 2400,
                "confidence": 1.0,
                "needsReview": false
            }]
        });
        let lines: Vec<AlignmentLine> = from_arg(&args, "lines").unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].start_ms, 1000);
        assert_eq!(lines[0].end_ms, 2400);
    }

    #[test]
    fn from_arg_missing_key_is_error() {
        let result: Result<Vec<AlignmentLine>, _> = from_arg(&json!({}), "lines");
        assert!(result.is_err());
    }
}
