//! ボイスチャンネルに繋いでいた時間を覚えておく。
//! 誰もいなくなって自動退出するときに「どのくらい参加していたか」を出すのに使う。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use poise::serenity_prelude::GuildId;
use tokio::sync::Mutex;

/// ギルドごとの接続開始時刻。
#[derive(Default)]
pub struct JoinTimes(Mutex<HashMap<GuildId, Instant>>);

impl JoinTimes {
    /// `/join` が成功したら呼ぶ。
    pub async fn set(&self, guild_id: GuildId) {
        self.0.lock().await.insert(guild_id, Instant::now());
    }

    /// 退出時に呼ぶ。呼んだ分は忘れるので、次に `set` するまでは None になる。
    pub async fn take(&self, guild_id: GuildId) -> Option<Duration> {
        self.0
            .lock()
            .await
            .remove(&guild_id)
            .map(|joined| joined.elapsed())
    }
}

/// 「3時間26分」のような表示にする。1分未満は秒で出す。
pub fn format_duration(duration: Duration) -> String {
    let total = duration.as_secs();
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}時間{minutes}分")
    } else if minutes > 0 {
        format!("{minutes}分")
    } else {
        format!("{seconds}秒")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_hours_minutes_and_seconds() {
        assert_eq!(format_duration(Duration::from_secs(30)), "30秒");
        assert_eq!(format_duration(Duration::from_secs(90)), "1分");
        assert_eq!(
            format_duration(Duration::from_secs(3 * 3600 + 26 * 60)),
            "3時間26分"
        );
    }

    #[tokio::test]
    async fn take_returns_elapsed_and_forgets() {
        let times = JoinTimes::default();
        let guild = GuildId::new(1);

        assert!(times.take(guild).await.is_none());

        times.set(guild).await;
        assert!(times.take(guild).await.is_some());
        assert!(times.take(guild).await.is_none());
    }
}
