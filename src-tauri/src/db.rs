use std::path::Path;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use crate::models::{AlignmentLine, CaptionLine, LyricLine, MemberProfile, SongPackage};

pub struct Repository {
    conn: Connection,
}

impl Repository {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        let repo = Self { conn };
        repo.migrate()?;
        Ok(repo)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS songs (
                id INTEGER PRIMARY KEY,
                title TEXT NOT NULL,
                artist TEXT NOT NULL,
                group_name TEXT,
                source_url TEXT,
                provider TEXT,
                created_at TEXT DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT DEFAULT CURRENT_TIMESTAMP,
                UNIQUE(title, artist)
            );
            CREATE TABLE IF NOT EXISTS lyric_lines (
                id INTEGER PRIMARY KEY,
                song_id INTEGER NOT NULL REFERENCES songs(id) ON DELETE CASCADE,
                line_index INTEGER NOT NULL,
                member TEXT,
                original TEXT NOT NULL,
                romanization TEXT,
                english TEXT,
                segments TEXT,
                UNIQUE(song_id, line_index)
            );
            CREATE TABLE IF NOT EXISTS videos (
                video_id TEXT PRIMARY KEY,
                title TEXT,
                artist_hint TEXT,
                original_url TEXT,
                updated_at TEXT DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE IF NOT EXISTS caption_lines (
                id INTEGER PRIMARY KEY,
                video_id TEXT NOT NULL,
                line_index INTEGER NOT NULL,
                start_ms INTEGER NOT NULL,
                end_ms INTEGER NOT NULL,
                text TEXT NOT NULL,
                UNIQUE(video_id, line_index)
            );
            CREATE TABLE IF NOT EXISTS alignments (
                song_id INTEGER NOT NULL REFERENCES songs(id) ON DELETE CASCADE,
                video_id TEXT NOT NULL,
                lyric_index INTEGER NOT NULL,
                caption_index INTEGER,
                start_ms INTEGER NOT NULL,
                end_ms INTEGER NOT NULL,
                confidence REAL NOT NULL,
                needs_review INTEGER NOT NULL,
                PRIMARY KEY(song_id, video_id, lyric_index)
            );
            CREATE TABLE IF NOT EXISTS members (
                id INTEGER PRIMARY KEY,
                group_name TEXT NOT NULL,
                stage_name TEXT NOT NULL,
                real_name TEXT,
                color TEXT NOT NULL,
                image_url TEXT,
                local_image_path TEXT,
                provider TEXT,
                is_override INTEGER DEFAULT 0,
                UNIQUE(group_name, stage_name)
            );
            CREATE TABLE IF NOT EXISTS provider_metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE IF NOT EXISTS user_overrides (
                id INTEGER PRIMARY KEY,
                entity_type TEXT NOT NULL,
                entity_key TEXT NOT NULL,
                value TEXT NOT NULL,
                updated_at TEXT DEFAULT CURRENT_TIMESTAMP,
                UNIQUE(entity_type, entity_key)
            );
            "#,
        )?;
        ensure_column(&self.conn, "lyric_lines", "segments", "TEXT")?;
        ensure_column(&self.conn, "lyric_lines", "includes_all", "INTEGER NOT NULL DEFAULT 0")?;
        Ok(())
    }

    pub fn upsert_song_package(&mut self, package: &mut SongPackage) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            r#"
            INSERT INTO songs (title, artist, group_name, source_url, provider, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, CURRENT_TIMESTAMP)
            ON CONFLICT(title, artist) DO UPDATE SET
                group_name=excluded.group_name,
                source_url=excluded.source_url,
                provider=excluded.provider,
                updated_at=CURRENT_TIMESTAMP
            "#,
            params![
                package.song.title,
                package.song.artist,
                package.song.group_name,
                package.song.source_url,
                package.provider
            ],
        )?;
        let song_id: i64 = tx.query_row(
            "SELECT id FROM songs WHERE title=?1 AND artist=?2",
            params![package.song.title, package.song.artist],
            |row| row.get(0),
        )?;
        package.song.id = Some(song_id);
        tx.execute("DELETE FROM lyric_lines WHERE song_id=?1", params![song_id])?;
        for line in &mut package.lines {
            line.song_id = Some(song_id);
            tx.execute(
                r#"
                INSERT INTO lyric_lines (song_id, line_index, member, original, romanization, english, segments, includes_all)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
                params![
                    song_id,
                    line.index as i64,
                    line.member,
                    line.original,
                    line.romanization,
                    line.english,
                    serde_json::to_string(&line.segments)?,
                    line.with_all as i64,
                ],
            )?;
            line.id = Some(tx.last_insert_rowid());
        }
        if let Some(group_name) = &package.song.group_name {
            upsert_members_tx(&tx, group_name, &package.members)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn lyric_lines(&self, song_id: i64) -> Result<Vec<LyricLine>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, song_id, line_index, member, original, romanization, english, segments, includes_all FROM lyric_lines WHERE song_id=?1 ORDER BY line_index",
        )?;
        let rows = stmt.query_map(params![song_id], |row| {
            let raw_segments: Option<String> = row.get(7)?;
            Ok(LyricLine {
                id: row.get(0)?,
                song_id: row.get(1)?,
                index: row.get::<_, i64>(2)? as usize,
                member: row.get(3)?,
                original: row.get(4)?,
                romanization: row.get(5)?,
                english: row.get(6)?,
                with_all: row.get::<_, i64>(8)? != 0,
                segments: raw_segments
                    .and_then(|value| serde_json::from_str(&value).ok())
                    .unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn upsert_caption_lines(&mut self, video_id: &str, captions: &[CaptionLine]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM caption_lines WHERE video_id=?1",
            params![video_id],
        )?;
        for caption in captions {
            tx.execute(
                r#"
                INSERT INTO caption_lines (video_id, line_index, start_ms, end_ms, text)
                VALUES (?1, ?2, ?3, ?4, ?5)
                "#,
                params![
                    video_id,
                    caption.index as i64,
                    caption.start_ms,
                    caption.end_ms,
                    caption.text
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn caption_lines(&self, video_id: &str) -> Result<Vec<CaptionLine>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, video_id, line_index, start_ms, end_ms, text FROM caption_lines WHERE video_id=?1 ORDER BY line_index",
        )?;
        let rows = stmt.query_map(params![video_id], |row| {
            Ok(CaptionLine {
                id: row.get(0)?,
                video_id: row.get(1)?,
                index: row.get::<_, i64>(2)? as usize,
                start_ms: row.get(3)?,
                end_ms: row.get(4)?,
                text: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn upsert_alignment(
        &mut self,
        song_id: i64,
        video_id: &str,
        lines: &[AlignmentLine],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        for line in lines {
            tx.execute(
                r#"
                INSERT INTO alignments (song_id, video_id, lyric_index, caption_index, start_ms, end_ms, confidence, needs_review)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                ON CONFLICT(song_id, video_id, lyric_index) DO UPDATE SET
                    caption_index=excluded.caption_index,
                    start_ms=excluded.start_ms,
                    end_ms=excluded.end_ms,
                    confidence=excluded.confidence,
                    needs_review=excluded.needs_review
                "#,
                params![
                    song_id,
                    video_id,
                    line.lyric_index as i64,
                    line.caption_index.map(|idx| idx as i64),
                    line.start_ms,
                    line.end_ms,
                    line.confidence,
                    line.needs_review as i64
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn upsert_members(&mut self, group_name: &str, members: &[MemberProfile]) -> Result<()> {
        let tx = self.conn.transaction()?;
        upsert_members_tx(&tx, group_name, members)?;
        tx.commit()?;
        Ok(())
    }

    pub fn save_member_override(&mut self, group_name: &str, member: &MemberProfile) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO members (group_name, stage_name, real_name, color, image_url, local_image_path, provider, is_override)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)
            ON CONFLICT(group_name, stage_name) DO UPDATE SET
                real_name=excluded.real_name,
                color=excluded.color,
                image_url=excluded.image_url,
                local_image_path=excluded.local_image_path,
                provider=excluded.provider,
                is_override=1
            "#,
            params![
                group_name,
                member.stage_name,
                member.real_name,
                member.color,
                member.image_url,
                member.local_image_path,
                member.provider
            ],
        )?;
        let value = serde_json::to_string(member)?;
        self.conn.execute(
            r#"
            INSERT INTO user_overrides (entity_type, entity_key, value, updated_at)
            VALUES ('member', ?1, ?2, CURRENT_TIMESTAMP)
            ON CONFLICT(entity_type, entity_key) DO UPDATE SET value=excluded.value, updated_at=CURRENT_TIMESTAMP
            "#,
            params![format!("{group_name}:{}", member.stage_name), value],
        )?;
        Ok(())
    }

    pub fn get_user_setting(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM user_overrides WHERE entity_type='setting' AND entity_key=?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_user_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO user_overrides (entity_type, entity_key, value, updated_at)
            VALUES ('setting', ?1, ?2, CURRENT_TIMESTAMP)
            ON CONFLICT(entity_type, entity_key) DO UPDATE SET
                value=excluded.value,
                updated_at=CURRENT_TIMESTAMP
            "#,
            params![key, value],
        )?;
        Ok(())
    }

    #[cfg(test)]
    pub fn alignment_count(&self, song_id: i64, video_id: &str) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM alignments WHERE song_id=?1 AND video_id=?2",
            params![song_id, video_id],
            |row| row.get(0),
        )?)
    }

    pub fn alignment_lines(&self, song_id: i64, video_id: &str) -> Result<Vec<AlignmentLine>> {
        let mut stmt = self.conn.prepare(
            "SELECT lyric_index, caption_index, start_ms, end_ms, confidence, needs_review FROM alignments WHERE song_id=?1 AND video_id=?2 ORDER BY lyric_index",
        )?;
        let rows = stmt.query_map(params![song_id, video_id], |row| {
            Ok(AlignmentLine {
                lyric_index: row.get::<_, i64>(0)? as usize,
                caption_index: row
                    .get::<_, Option<i64>>(1)?
                    .map(|index| index as usize),
                start_ms: row.get(2)?,
                end_ms: row.get(3)?,
                confidence: row.get(4)?,
                needs_review: row.get::<_, i64>(5)? != 0,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    #[allow(dead_code)]
    pub fn song_id(&self, title: &str, artist: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM songs WHERE title=?1 AND artist=?2",
                params![title, artist],
                |row| row.get(0),
            )
            .optional()?)
    }
}

fn upsert_members_tx(
    tx: &rusqlite::Transaction<'_>,
    group_name: &str,
    members: &[MemberProfile],
) -> Result<()> {
    for member in members {
        tx.execute(
            r#"
            INSERT INTO members (group_name, stage_name, real_name, color, image_url, local_image_path, provider)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(group_name, stage_name) DO UPDATE SET
                real_name=COALESCE(members.real_name, excluded.real_name),
                color=CASE WHEN members.is_override = 1 THEN members.color ELSE excluded.color END,
                image_url=COALESCE(members.image_url, excluded.image_url),
                local_image_path=COALESCE(members.local_image_path, excluded.local_image_path),
                provider=excluded.provider
            "#,
            params![
                group_name,
                member.stage_name,
                member.real_name,
                member.color,
                member.image_url,
                member.local_image_path,
                member.provider
            ],
        )?;
    }
    Ok(())
}

fn ensure_column(
    conn: &Connection,
    table_name: &str,
    column_name: &str,
    column_type: &str,
) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table_name})"))?;
    let exists = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .any(|name| name == column_name);
    if !exists {
        conn.execute(
            &format!("ALTER TABLE {table_name} ADD COLUMN {column_name} {column_type}"),
            [],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::NamedTempFile;

    use super::Repository;
    use crate::models::{AlignmentLine, CaptionLine};

    #[test]
    fn caches_song_captions_alignment_and_overrides() {
        let file = NamedTempFile::new().unwrap();
        let mut repo = Repository::open(file.path()).unwrap();
        let mut package =
            crate::lyrics::parse_manual_lyrics("A: hello\nB: world", "Song", "Group").unwrap();
        repo.upsert_song_package(&mut package).unwrap();
        let song_id = package.song.id.unwrap();
        assert_eq!(repo.lyric_lines(song_id).unwrap().len(), 2);

        repo.upsert_caption_lines(
            "video",
            &[CaptionLine {
                id: None,
                video_id: "video".into(),
                index: 0,
                start_ms: 10,
                end_ms: 20,
                text: "hello".into(),
            }],
        )
        .unwrap();
        assert_eq!(repo.caption_lines("video").unwrap().len(), 1);

        repo.upsert_alignment(
            song_id,
            "video",
            &[AlignmentLine {
                lyric_index: 0,
                caption_index: Some(0),
                start_ms: 10,
                end_ms: 20,
                confidence: 1.0,
                needs_review: false,
            }],
        )
        .unwrap();
        assert_eq!(repo.alignment_count(song_id, "video").unwrap(), 1);

        let mut member = package.members[0].clone();
        member.color = "#111111".into();
        repo.save_member_override("Group", &member).unwrap();
    }
}
