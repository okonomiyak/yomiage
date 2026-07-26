//! 音楽再生（yt-dlp）。
//!
//! 読み上げとは別のトラックとして流す。songbird は 1 つの Call に複数トラックを
//! 混ぜられるので、音楽の上に読み上げが乗る形になる。音量を既定で小さめにしてあるのはそのため。
//!
//! キューは songbird の `builtin-queue` に任せる。自前で持つと再生完了の検知と
//! 状態の同期を自分で書くことになり、ずれの原因になる。読み上げは `play_input` で
//! 直接鳴らしているのでキューには入らず、音楽とは独立して動く。

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context as _;
use poise::serenity_prelude::GuildId;
use songbird::Songbird;
use songbird::input::{Compose, YoutubeDl};
use songbird::tracks::TrackHandle;
use tokio::sync::Mutex;
use uuid::Uuid;

pub struct Manager {
    songbird: Arc<Songbird>,
    /// yt-dlp が返したストリーム URL を取りに行くのに使う。
    http: reqwest::Client,
    /// トラック UUID → 曲名。songbird 0.6 の `TrackHandle::data` は型が違うと
    /// panic するので使わない（読み上げのトラックと混ざる余地を残さない）。
    titles: Mutex<HashMap<Uuid, String>>,
}

/// キューに積んだ結果。
pub struct Queued {
    pub title: String,
    /// キューの何番目か。1 なら即再生。
    pub position: usize,
}

impl Manager {
    pub fn new(songbird: Arc<Songbird>, http: reqwest::Client) -> Self {
        Self {
            songbird,
            http,
            titles: Mutex::new(HashMap::new()),
        }
    }

    /// URL ならそのまま、そうでなければ検索語として扱う。空きがあれば即再生、なければ待機。
    pub async fn enqueue(
        &self,
        guild_id: GuildId,
        query: &str,
        volume: f32,
    ) -> anyhow::Result<Queued> {
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
            Ok(metadata) => metadata.title.unwrap_or_else(|| query.to_owned()),
            Err(error) => {
                tracing::debug!(%guild_id, %error, "failed to fetch metadata");
                query.to_owned()
            }
        };

        let (handle, position) = {
            let mut call = call_lock.lock().await;
            let handle = call.enqueue_input(source.into()).await;
            (handle, call.queue().len())
        };

        if let Err(error) = handle.set_volume(volume) {
            tracing::warn!(%guild_id, %error, "failed to apply volume");
        }
        self.titles
            .lock()
            .await
            .insert(handle.uuid(), title.clone());

        tracing::info!(%guild_id, position, title = title.as_str(), "music queued");
        Ok(Queued { title, position })
    }

    /// (再生中, 待機中) のタイトル一覧。
    pub async fn queue(&self, guild_id: GuildId) -> Vec<String> {
        let Some(call_lock) = self.songbird.get(guild_id) else {
            return Vec::new();
        };
        let handles: Vec<TrackHandle> = {
            let call = call_lock.lock().await;
            call.queue().current_queue()
        };

        let mut known = self.titles.lock().await;
        // キューから消えた曲の名前は捨てる。放っておくと溜まり続ける。
        known.retain(|uuid, _| handles.iter().any(|handle| handle.uuid() == *uuid));

        handles
            .iter()
            .map(|handle| {
                known
                    .get(&handle.uuid())
                    .cloned()
                    .unwrap_or_else(|| "（タイトル不明）".to_owned())
            })
            .collect()
    }

    /// 今の曲を飛ばして次へ。飛ばすものがあれば true。
    pub async fn skip(&self, guild_id: GuildId) -> bool {
        let Some(call_lock) = self.songbird.get(guild_id) else {
            return false;
        };
        let call = call_lock.lock().await;
        if call.queue().is_empty() {
            return false;
        }
        match call.queue().skip() {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%guild_id, %error, "failed to skip music");
                false
            }
        }
    }

    /// 全部止めてキューを空にする。止めるものがあれば true。
    pub async fn stop(&self, guild_id: GuildId) -> bool {
        let Some(call_lock) = self.songbird.get(guild_id) else {
            return false;
        };
        let call = call_lock.lock().await;
        if call.queue().is_empty() {
            return false;
        }
        call.queue().stop();
        drop(call);
        self.titles.lock().await.clear();
        tracing::debug!(%guild_id, "music queue cleared");
        true
    }

    /// 再生中と待機中の全部に音量を反映する。反映できた件数を返す。
    pub async fn set_volume(&self, guild_id: GuildId, volume: f32) -> usize {
        let Some(call_lock) = self.songbird.get(guild_id) else {
            return 0;
        };
        let handles: Vec<TrackHandle> = {
            let call = call_lock.lock().await;
            call.queue().current_queue()
        };

        handles
            .iter()
            .filter(|handle| handle.set_volume(volume).is_ok())
            .count()
    }
}
