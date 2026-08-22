//! 曲がキューの中で自動的に切り替わったときのアナウンス。
//!
//! 手動 `/play` の直後は既にコマンドの返信で分かるので、ここで流すのは
//! **キューの自動進行だけ**（前の曲が終わって次が始まった、または尽きた）。
//! 投稿先は `/join` したテキストチャンネル（ギルドごとに直近の 1 つだけ覚える）。

use std::collections::HashMap;
use std::sync::Arc;

use poise::serenity_prelude as serenity;
use tokio::sync::{Mutex, mpsc};

use yomiage_bot::music::TrackChanged;

/// ギルドごとに、アナウンスを流すテキストチャンネル。
#[derive(Default)]
pub struct AnnounceChannels(Mutex<HashMap<serenity::GuildId, serenity::ChannelId>>);

impl AnnounceChannels {
    /// `/join` のたびに呼ぶ。直近に `/join` したチャンネルへ流す。
    pub async fn set(&self, guild_id: serenity::GuildId, channel_id: serenity::ChannelId) {
        self.0.lock().await.insert(guild_id, channel_id);
    }

    async fn get(&self, guild_id: serenity::GuildId) -> Option<serenity::ChannelId> {
        self.0.lock().await.get(&guild_id).copied()
    }
}

/// 自動進行の通知を受け取り続け、投稿先が分かっていれば流すタスク。
/// `Manager::new` が返す受信側を渡して起動する。
pub async fn run(
    mut changes: mpsc::UnboundedReceiver<TrackChanged>,
    channels: Arc<AnnounceChannels>,
    http: Arc<serenity::Http>,
) {
    while let Some(change) = changes.recv().await {
        let Some(channel_id) = channels.get(change.guild_id).await else {
            continue;
        };

        let body = match change.next {
            Some(next) => format!(
                "⏹ {} の再生が終わりました。\n▶ {} を再生開始します。",
                change.finished.link(),
                next.link()
            ),
            None => format!(
                "⏹ {} の再生が終わりました。キューは空です。",
                change.finished.link()
            ),
        };

        if let Err(error) = channel_id.say(&http, body).await {
            tracing::warn!(guild_id = %change.guild_id, %error, "failed to announce track change");
        }
    }
}
