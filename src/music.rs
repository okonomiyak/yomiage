//! 音楽再生（yt-dlp）。
//!
//! 読み上げとは別のトラックとして流す。songbird は 1 つの Call に複数トラックを
//! 混ぜられるので、音楽の上に読み上げが乗る形になる。音量を既定で小さめにしてあるのはそのため。
//!
//! ギルドにつき 1 曲だけ持つ。`/play` は前の曲を止めて差し替える。

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context as _;
use poise::serenity_prelude::GuildId;
use songbird::Songbird;
use songbird::input::{Compose, YoutubeDl};
use songbird::tracks::TrackHandle;
use tokio::sync::Mutex;

pub struct Manager {
    songbird: Arc<Songbird>,
    /// yt-dlp が返したストリーム URL を取りに行くのに使う。
    http: reqwest::Client,
    playing: Mutex<HashMap<GuildId, TrackHandle>>,
}

/// 再生開始時に分かった情報。返信に使うだけ。
pub struct NowPlaying {
    pub title: Option<String>,
}

impl Manager {
    pub fn new(songbird: Arc<Songbird>, http: reqwest::Client) -> Self {
        Self {
            songbird,
            http,
            playing: Mutex::new(HashMap::new()),
        }
    }

    /// URL ならそのまま、そうでなければ検索語として扱う。
    pub async fn play(
        &self,
        guild_id: GuildId,
        query: &str,
        volume: f32,
    ) -> anyhow::Result<NowPlaying> {
        let call_lock = self
            .songbird
            .get(guild_id)
            .context("ボイスチャンネルに接続していない")?;

        let mut source = if query.starts_with("http://") || query.starts_with("https://") {
            YoutubeDl::new(self.http.clone(), query.to_owned())
        } else {
            YoutubeDl::new_search(self.http.clone(), query.to_owned())
        };

        // タイトルは取れなくても再生は続ける（yt-dlp をもう一度叩くだけなので失敗しうる）。
        let title = match source.aux_metadata().await {
            Ok(metadata) => metadata.title,
            Err(error) => {
                tracing::debug!(%guild_id, %error, "failed to fetch metadata");
                None
            }
        };

        self.stop(guild_id).await;

        let track = {
            let mut call = call_lock.lock().await;
            // play_input は既存トラックを止めない。読み上げと混ざるのが狙い。
            call.play_input(source.into())
        };
        if let Err(error) = track.set_volume(volume) {
            tracing::warn!(%guild_id, %error, "failed to apply volume");
        }
        self.playing.lock().await.insert(guild_id, track);

        tracing::info!(%guild_id, volume, title = title.as_deref().unwrap_or("?"), "music started");
        Ok(NowPlaying { title })
    }

    /// 止めるものがあれば true。
    pub async fn stop(&self, guild_id: GuildId) -> bool {
        let Some(track) = self.playing.lock().await.remove(&guild_id) else {
            return false;
        };
        let _ = track.stop();
        tracing::debug!(%guild_id, "music stopped");
        true
    }

    /// 再生中の曲に音量を反映する。流れていなければ false（設定自体は呼び出し側で保存する）。
    pub async fn set_volume(&self, guild_id: GuildId, volume: f32) -> bool {
        let playing = self.playing.lock().await;
        let Some(track) = playing.get(&guild_id) else {
            return false;
        };
        match track.set_volume(volume) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%guild_id, %error, "failed to change volume");
                false
            }
        }
    }
}
