//! SQLite 永続化（PLAN §9）。
//!
//! Discord の ID は u64、SQLite の INTEGER は i64 なので入口と出口で詰め替える。
//! スノーフレークは 2^63 未満なので往復しても値は壊れない。

use std::str::FromStr;

use anyhow::Context as _;
use poise::serenity_prelude::{ChannelId, GuildId, UserId};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::voicevox::{StyleId, Voice};

/// ギルドごとの設定（PLAN §9 の `guild_settings`）。
#[derive(Debug, Clone)]
pub struct GuildSettings {
    pub max_length: usize,
    pub read_bots: bool,
    pub ignore_prefix: String,
    /// 音楽の音量（0.0〜1.0）。読み上げに被せるので既定は小さめ。
    pub music_volume: f32,
    pub tts_enabled: bool,
    pub music_enabled: bool,
}

impl Default for GuildSettings {
    fn default() -> Self {
        Self {
            max_length: 100,
            read_bots: false,
            ignore_prefix: ";".to_owned(),
            music_volume: 0.3,
            tts_enabled: true,
            music_enabled: true,
        }
    }
}

pub struct Db {
    pool: SqlitePool,
}

impl Db {
    /// 接続してマイグレーションを流す。ファイルが無ければ作る。
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let options = SqliteConnectOptions::from_str(url)
            .with_context(|| format!("DATABASE_URL が不正: {url}"))?
            .create_if_missing(true)
            // 読み書きが並行するので WAL にしておく。
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);

        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .context("SQLite に接続できない")?;

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("マイグレーションに失敗")?;

        Ok(Self { pool })
    }

    // ---- 読み上げ対象チャンネル（§13-3 で複数登録可）----

    pub async fn add_read_channel(
        &self,
        guild_id: GuildId,
        channel_id: ChannelId,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO read_channels (guild_id, channel_id) VALUES (?, ?)
             ON CONFLICT (guild_id, channel_id) DO NOTHING",
        )
        .bind(id(guild_id.get()))
        .bind(id(channel_id.get()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 全ギルドの登録を消す。起動時に呼ぶ：再起動直後はどの VC にも居ないので、
    /// 登録だけ残っていると「VC に居ないのに合成する」状態になる。
    pub async fn clear_all_read_channels(&self) -> anyhow::Result<u64> {
        let result = sqlx::query("DELETE FROM read_channels")
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    pub async fn clear_read_channels(&self, guild_id: GuildId) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM read_channels WHERE guild_id = ?")
            .bind(id(guild_id.get()))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 1 チャンネルだけ読み上げ対象から外す（`/unbind` 用、PLAN §13-A）。
    pub async fn remove_read_channel(
        &self,
        guild_id: GuildId,
        channel_id: ChannelId,
    ) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM read_channels WHERE guild_id = ? AND channel_id = ?")
            .bind(id(guild_id.get()))
            .bind(id(channel_id.get()))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn is_read_channel(
        &self,
        guild_id: GuildId,
        channel_id: ChannelId,
    ) -> anyhow::Result<bool> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT 1 FROM read_channels WHERE guild_id = ? AND channel_id = ?")
                .bind(id(guild_id.get()))
                .bind(id(channel_id.get()))
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.is_some())
    }

    pub async fn read_channels(&self, guild_id: GuildId) -> anyhow::Result<Vec<ChannelId>> {
        let rows: Vec<(i64,)> =
            sqlx::query_as("SELECT channel_id FROM read_channels WHERE guild_id = ?")
                .bind(id(guild_id.get()))
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .map(|(channel_id,)| ChannelId::new(channel_id as u64))
            .collect())
    }

    // ---- テキスト ch と VC の紐づけ ----
    //
    // read_channels と違い、これは設定なので起動時に消さない。

    pub async fn bind_channel(
        &self,
        guild_id: GuildId,
        text_channel_id: ChannelId,
        voice_channel_id: ChannelId,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO channel_bindings (guild_id, text_channel_id, voice_channel_id)
             VALUES (?, ?, ?)
             ON CONFLICT (guild_id, text_channel_id)
             DO UPDATE SET voice_channel_id = excluded.voice_channel_id",
        )
        .bind(id(guild_id.get()))
        .bind(id(text_channel_id.get()))
        .bind(id(voice_channel_id.get()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 消せたら true。無かったら false。
    pub async fn unbind_channel(
        &self,
        guild_id: GuildId,
        text_channel_id: ChannelId,
    ) -> anyhow::Result<bool> {
        let result =
            sqlx::query("DELETE FROM channel_bindings WHERE guild_id = ? AND text_channel_id = ?")
                .bind(id(guild_id.get()))
                .bind(id(text_channel_id.get()))
                .execute(&self.pool)
                .await?;
        Ok(result.rows_affected() > 0)
    }

    /// ギルドの紐づけ一覧（テキスト ch, ボイス ch）。
    pub async fn bindings(&self, guild_id: GuildId) -> anyhow::Result<Vec<(ChannelId, ChannelId)>> {
        let rows: Vec<(i64, i64)> = sqlx::query_as(
            "SELECT text_channel_id, voice_channel_id FROM channel_bindings WHERE guild_id = ?",
        )
        .bind(id(guild_id.get()))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(text, voice)| (ChannelId::new(text as u64), ChannelId::new(voice as u64)))
            .collect())
    }

    /// この VC に紐づいたテキスト ch。`/join` したときに読み上げ対象へ入れる。
    pub async fn bound_text_channels(
        &self,
        guild_id: GuildId,
        voice_channel_id: ChannelId,
    ) -> anyhow::Result<Vec<ChannelId>> {
        let rows: Vec<(i64,)> = sqlx::query_as(
            "SELECT text_channel_id FROM channel_bindings
             WHERE guild_id = ? AND voice_channel_id = ?",
        )
        .bind(id(guild_id.get()))
        .bind(id(voice_channel_id.get()))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(text,)| ChannelId::new(text as u64))
            .collect())
    }

    /// このテキスト ch に紐づいた VC。`/join` の接続先を決めるのに使う。
    pub async fn bound_voice_channel(
        &self,
        guild_id: GuildId,
        text_channel_id: ChannelId,
    ) -> anyhow::Result<Option<ChannelId>> {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT voice_channel_id FROM channel_bindings
             WHERE guild_id = ? AND text_channel_id = ?",
        )
        .bind(id(guild_id.get()))
        .bind(id(text_channel_id.get()))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(voice,)| ChannelId::new(voice as u64)))
    }

    // ---- ギルド設定 ----

    /// 行が無ければ既定値を返す。`/join` した時点では行を作らない。
    pub async fn guild_settings(&self, guild_id: GuildId) -> anyhow::Result<GuildSettings> {
        let row = sqlx::query(
            "SELECT max_length, read_bots, ignore_prefix, music_volume, tts_enabled, music_enabled
             FROM guild_settings WHERE guild_id = ?",
        )
        .bind(id(guild_id.get()))
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(GuildSettings::default());
        };
        Ok(GuildSettings {
            max_length: row.try_get::<i64, _>("max_length")?.max(1) as usize,
            read_bots: row.try_get::<i64, _>("read_bots")? != 0,
            ignore_prefix: row.try_get("ignore_prefix")?,
            music_volume: row.try_get::<f64, _>("music_volume")? as f32,
            tts_enabled: row.try_get::<i64, _>("tts_enabled")? != 0,
            music_enabled: row.try_get::<i64, _>("music_enabled")? != 0,
        })
    }

    /// 読み上げ文字数の上限を変える。行が無ければ他の列は既定値で作る。
    pub async fn set_max_length(&self, guild_id: GuildId, max_length: usize) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO guild_settings (guild_id, max_length) VALUES (?, ?)
             ON CONFLICT (guild_id) DO UPDATE SET max_length = excluded.max_length",
        )
        .bind(id(guild_id.get()))
        .bind(max_length as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 機能ごとの有効・無効。列名は呼び出し側の固定文字列のみ（外部入力を混ぜない）。
    async fn set_feature(&self, guild_id: GuildId, column: &str, on: bool) -> anyhow::Result<()> {
        let sql = format!(
            "INSERT INTO guild_settings (guild_id, {column}) VALUES (?, ?)
             ON CONFLICT (guild_id) DO UPDATE SET {column} = excluded.{column}"
        );
        sqlx::query(&sql)
            .bind(id(guild_id.get()))
            .bind(i64::from(on))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_tts_enabled(&self, guild_id: GuildId, on: bool) -> anyhow::Result<()> {
        self.set_feature(guild_id, "tts_enabled", on).await
    }

    pub async fn set_music_enabled(&self, guild_id: GuildId, on: bool) -> anyhow::Result<()> {
        self.set_feature(guild_id, "music_enabled", on).await
    }

    pub async fn set_music_volume(&self, guild_id: GuildId, volume: f32) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO guild_settings (guild_id, music_volume) VALUES (?, ?)
             ON CONFLICT (guild_id) DO UPDATE SET music_volume = excluded.music_volume",
        )
        .bind(id(guild_id.get()))
        .bind(f64::from(volume))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---- ユーザーごとの声（§13-2 でギルド横断）----

    pub async fn voice(&self, user_id: UserId) -> anyhow::Result<Voice> {
        let row = sqlx::query(
            "SELECT speaker, speed, pitch, intonation FROM user_voice WHERE user_id = ?",
        )
        .bind(id(user_id.get()))
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(Voice::default());
        };
        Ok(Voice {
            style: StyleId(
                row.try_get::<i64, _>("speaker")?
                    .clamp(0, i64::from(u32::MAX)) as u32,
            ),
            speed: row.try_get::<f64, _>("speed")? as f32,
            pitch: row.try_get::<f64, _>("pitch")? as f32,
            intonation: row.try_get::<f64, _>("intonation")? as f32,
        })
    }

    pub async fn set_style(&self, user_id: UserId, style: StyleId) -> anyhow::Result<()> {
        self.upsert_voice(user_id, "speaker", f64::from(style.0))
            .await
    }

    pub async fn set_speed(&self, user_id: UserId, value: f32) -> anyhow::Result<()> {
        self.upsert_voice(user_id, "speed", f64::from(value)).await
    }

    pub async fn set_pitch(&self, user_id: UserId, value: f32) -> anyhow::Result<()> {
        self.upsert_voice(user_id, "pitch", f64::from(value)).await
    }

    pub async fn set_intonation(&self, user_id: UserId, value: f32) -> anyhow::Result<()> {
        self.upsert_voice(user_id, "intonation", f64::from(value))
            .await
    }

    // ---- サーバー辞書（PLAN §7-4）----

    /// 表記 → 読み。ENGINE のユーザー辞書ではなくテキスト置換で実現する（§7-4 の判断）。
    pub async fn dictionary(&self, guild_id: GuildId) -> anyhow::Result<Vec<(String, String)>> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT surface, reading FROM dictionary WHERE guild_id = ?")
                .bind(id(guild_id.get()))
                .fetch_all(&self.pool)
                .await?;
        Ok(rows)
    }

    pub async fn add_dictionary_entry(
        &self,
        guild_id: GuildId,
        surface: &str,
        reading: &str,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO dictionary (guild_id, surface, reading) VALUES (?, ?, ?)
             ON CONFLICT (guild_id, surface) DO UPDATE SET reading = excluded.reading",
        )
        .bind(id(guild_id.get()))
        .bind(surface)
        .bind(reading)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 消せたら true。無かったら false。
    pub async fn remove_dictionary_entry(
        &self,
        guild_id: GuildId,
        surface: &str,
    ) -> anyhow::Result<bool> {
        let result = sqlx::query("DELETE FROM dictionary WHERE guild_id = ? AND surface = ?")
            .bind(id(guild_id.get()))
            .bind(surface)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 列名は呼び出し側の固定文字列のみ（外部入力を混ぜない）。
    async fn upsert_voice(&self, user_id: UserId, column: &str, value: f64) -> anyhow::Result<()> {
        let sql = format!(
            "INSERT INTO user_voice (user_id, {column}) VALUES (?, ?)
             ON CONFLICT (user_id) DO UPDATE SET {column} = excluded.{column}"
        );
        sqlx::query(&sql)
            .bind(id(user_id.get()))
            .bind(value)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// u64 の Discord ID を SQLite の INTEGER に詰める。
fn id(value: u64) -> i64 {
    value as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn memory_db() -> Db {
        // ファイルを作らずにスキーマとクエリを検証する。
        Db::connect("sqlite::memory:").await.expect("接続に失敗")
    }

    #[tokio::test]
    async fn defaults_are_returned_when_no_row_exists() {
        let db = memory_db().await;

        let voice = db.voice(UserId::new(1)).await.expect("取得に失敗");
        assert_eq!(voice, Voice::default());

        let settings = db
            .guild_settings(GuildId::new(1))
            .await
            .expect("取得に失敗");
        assert_eq!(settings.max_length, 100);
        assert!(!settings.read_bots);
        assert_eq!(settings.ignore_prefix, ";");
    }

    #[tokio::test]
    async fn voice_settings_are_updated_per_column() {
        let db = memory_db().await;
        let user = UserId::new(42);

        db.set_style(user, StyleId(8)).await.expect("失敗");
        db.set_speed(user, 1.5).await.expect("失敗");

        let voice = db.voice(user).await.expect("失敗");
        assert_eq!(voice.style, StyleId(8));
        assert_eq!(voice.speed, 1.5);
        // 触っていない列は既定値のまま。
        assert_eq!(voice.pitch, 0.0);
        assert_eq!(voice.intonation, 1.0);
    }

    #[tokio::test]
    async fn read_channels_are_added_and_cleared() {
        let db = memory_db().await;
        let guild = GuildId::new(7);
        let a = ChannelId::new(100);
        let b = ChannelId::new(200);

        db.add_read_channel(guild, a).await.expect("失敗");
        db.add_read_channel(guild, b).await.expect("失敗");
        // 二重登録しても壊れない。
        db.add_read_channel(guild, a).await.expect("失敗");

        assert!(db.is_read_channel(guild, a).await.expect("失敗"));
        assert!(
            !db.is_read_channel(guild, ChannelId::new(999))
                .await
                .expect("失敗")
        );
        assert_eq!(db.read_channels(guild).await.expect("失敗").len(), 2);

        db.clear_read_channels(guild).await.expect("失敗");
        assert!(db.read_channels(guild).await.expect("失敗").is_empty());
        assert!(!db.is_read_channel(guild, a).await.expect("失敗"));
    }

    #[tokio::test]
    async fn dictionary_entries_round_trip() {
        let db = memory_db().await;
        let guild = GuildId::new(1);

        db.add_dictionary_entry(guild, "VOICEVOX", "ボイスボックス")
            .await
            .expect("失敗");
        assert_eq!(
            db.dictionary(guild).await.expect("失敗"),
            vec![("VOICEVOX".to_owned(), "ボイスボックス".to_owned())]
        );

        // 同じ表記を登録し直したら読みが上書きされる（重複行にはならない）。
        db.add_dictionary_entry(guild, "VOICEVOX", "ボイボ")
            .await
            .expect("失敗");
        assert_eq!(
            db.dictionary(guild).await.expect("失敗"),
            vec![("VOICEVOX".to_owned(), "ボイボ".to_owned())]
        );

        assert!(
            db.remove_dictionary_entry(guild, "VOICEVOX")
                .await
                .expect("失敗")
        );
        assert!(
            !db.remove_dictionary_entry(guild, "VOICEVOX")
                .await
                .expect("失敗")
        );
        assert!(db.dictionary(guild).await.expect("失敗").is_empty());
    }

    #[tokio::test]
    async fn max_length_is_updated_without_touching_other_settings() {
        let db = memory_db().await;
        let guild = GuildId::new(1);

        db.set_max_length(guild, 300).await.expect("失敗");
        let settings = db.guild_settings(guild).await.expect("失敗");
        assert_eq!(settings.max_length, 300);
        // 行を新規作成しても他の列は既定値のまま。
        assert!(!settings.read_bots);
        assert_eq!(settings.ignore_prefix, ";");

        // 2 回目は更新になる。
        db.set_max_length(guild, 50).await.expect("失敗");
        assert_eq!(db.guild_settings(guild).await.expect("失敗").max_length, 50);
    }

    #[tokio::test]
    async fn feature_switches_default_to_on_and_are_independent() {
        let db = memory_db().await;
        let guild = GuildId::new(1);

        let settings = db.guild_settings(guild).await.expect("失敗");
        assert!(settings.tts_enabled);
        assert!(settings.music_enabled);

        db.set_tts_enabled(guild, false).await.expect("失敗");
        let settings = db.guild_settings(guild).await.expect("失敗");
        assert!(!settings.tts_enabled);
        // 片方を切ってももう片方は残る。
        assert!(settings.music_enabled);

        db.set_music_enabled(guild, false).await.expect("失敗");
        db.set_tts_enabled(guild, true).await.expect("失敗");
        let settings = db.guild_settings(guild).await.expect("失敗");
        assert!(settings.tts_enabled);
        assert!(!settings.music_enabled);
    }

    #[tokio::test]
    async fn music_volume_round_trips() {
        let db = memory_db().await;
        let guild = GuildId::new(1);

        assert_eq!(
            db.guild_settings(guild).await.expect("失敗").music_volume,
            0.3
        );

        db.set_music_volume(guild, 0.75).await.expect("失敗");
        assert_eq!(
            db.guild_settings(guild).await.expect("失敗").music_volume,
            0.75
        );

        // 音量を変えても他の設定は既定のまま。
        assert_eq!(
            db.guild_settings(guild).await.expect("失敗").max_length,
            100
        );
    }

    #[tokio::test]
    async fn max_length_is_per_guild() {
        let db = memory_db().await;
        db.set_max_length(GuildId::new(1), 300).await.expect("失敗");

        let other = db.guild_settings(GuildId::new(2)).await.expect("失敗");
        assert_eq!(other.max_length, 100);
    }

    #[tokio::test]
    async fn bindings_round_trip() {
        let db = memory_db().await;
        let guild = GuildId::new(1);
        let listen_only = ChannelId::new(10);
        let another_text = ChannelId::new(11);
        let voice = ChannelId::new(20);

        db.bind_channel(guild, listen_only, voice)
            .await
            .expect("失敗");
        db.bind_channel(guild, another_text, voice)
            .await
            .expect("失敗");

        assert_eq!(
            db.bound_voice_channel(guild, listen_only)
                .await
                .expect("失敗"),
            Some(voice)
        );
        let mut bound = db.bound_text_channels(guild, voice).await.expect("失敗");
        bound.sort();
        assert_eq!(bound, vec![listen_only, another_text]);

        // 貼り直すと上書きされる（1 テキスト ch につき VC は 1 つ）。
        let other_voice = ChannelId::new(21);
        db.bind_channel(guild, listen_only, other_voice)
            .await
            .expect("失敗");
        assert_eq!(
            db.bound_voice_channel(guild, listen_only)
                .await
                .expect("失敗"),
            Some(other_voice)
        );
        assert_eq!(db.bindings(guild).await.expect("失敗").len(), 2);

        assert!(db.unbind_channel(guild, listen_only).await.expect("失敗"));
        assert!(!db.unbind_channel(guild, listen_only).await.expect("失敗"));
        assert_eq!(
            db.bound_voice_channel(guild, listen_only)
                .await
                .expect("失敗"),
            None
        );
    }

    #[tokio::test]
    async fn bindings_survive_read_channel_reset() {
        // 起動時の read_channels クリアで紐づけまで消えてはいけない。
        let db = memory_db().await;
        let guild = GuildId::new(1);
        let text = ChannelId::new(10);
        let voice = ChannelId::new(20);

        db.bind_channel(guild, text, voice).await.expect("失敗");
        db.add_read_channel(guild, text).await.expect("失敗");

        db.clear_all_read_channels().await.expect("失敗");

        assert!(db.read_channels(guild).await.expect("失敗").is_empty());
        assert_eq!(
            db.bound_voice_channel(guild, text).await.expect("失敗"),
            Some(voice)
        );
    }

    #[tokio::test]
    async fn dictionaries_are_per_guild() {
        let db = memory_db().await;
        db.add_dictionary_entry(GuildId::new(1), "あ", "ア")
            .await
            .expect("失敗");

        assert!(
            db.dictionary(GuildId::new(2))
                .await
                .expect("失敗")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn other_guilds_are_not_affected() {
        let db = memory_db().await;
        let channel = ChannelId::new(1);
        db.add_read_channel(GuildId::new(1), channel)
            .await
            .expect("失敗");
        db.add_read_channel(GuildId::new(2), channel)
            .await
            .expect("失敗");

        db.clear_read_channels(GuildId::new(1)).await.expect("失敗");

        assert!(
            db.is_read_channel(GuildId::new(2), channel)
                .await
                .expect("失敗")
        );
    }
}
