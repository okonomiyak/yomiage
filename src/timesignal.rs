//! 時報。Bot が接続中の VC ごとに、毎分の境界をチェックして間隔と一致したら読み上げる。
//!
//! タイムゾーンは JST 固定（DST が無いので UTC+9 の固定オフセットで足りる）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use poise::serenity_prelude::GuildId;

use crate::db::Db;
use crate::speech::{self, SpeechTask};
use crate::voicevox::{StyleId, Voice};

/// JST は UTC+9 固定。
const JST_OFFSET_SECS: i64 = 9 * 3600;

/// 境界判定を待つ間隔。1 分より粗くしない。
const TICK: Duration = Duration::from_secs(60);

/// UNIX 秒から JST の (時, 分) を取り出す。
fn jst_hour_minute(unix_secs: i64) -> (u32, u32) {
    let day_secs = (unix_secs + JST_OFFSET_SECS).rem_euclid(86_400);
    ((day_secs / 3600) as u32, (day_secs % 3600 / 60) as u32)
}

/// この分が指定した頻度（30 か 60 分）の境界か。
fn is_boundary(minute: u32, interval_minutes: u32) -> bool {
    interval_minutes > 0 && minute.is_multiple_of(interval_minutes)
}

/// アナウンス文言。
fn announcement(hour: u32, minute: u32) -> String {
    if minute == 0 {
        format!("ただいま{hour}時です")
    } else {
        format!("ただいま{hour}時{minute}分です")
    }
}

/// 全ギルドを毎分チェックして、境界に当たったギルドで時報を読む。
/// Bot が接続していないギルドは `songbird` に現れないので自然に対象外になる。
pub async fn run(db: Arc<Db>, speech: Arc<speech::Manager>) {
    // 同じ境界で二度読まないための記録（tick がずれて同じ分に 2 回走った場合など）。
    let mut last: HashMap<GuildId, (u32, u32)> = HashMap::new();

    loop {
        tokio::time::sleep(TICK).await;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let (hour, minute) = jst_hour_minute(now);

        let guilds: Vec<GuildId> = speech
            .songbird()
            .iter()
            .map(|(guild_id, _)| GuildId::new(guild_id.0.get()))
            .collect();
        // 接続していないギルドの記録はもう使わないので捨てる。
        last.retain(|guild_id, _| guilds.contains(guild_id));

        for guild_id in guilds {
            if last.get(&guild_id) == Some(&(hour, minute)) {
                continue;
            }

            let settings = match db.guild_settings(guild_id).await {
                Ok(settings) => settings,
                Err(error) => {
                    tracing::warn!(%guild_id, %error, "failed to load guild settings for time signal");
                    continue;
                }
            };
            if !settings.time_signal_enabled || !is_boundary(minute, settings.time_signal_interval)
            {
                continue;
            }

            last.insert(guild_id, (hour, minute));
            tracing::info!(%guild_id, hour, minute, "time signal announced");
            speech
                .enqueue(
                    guild_id,
                    SpeechTask {
                        text: announcement(hour, minute),
                        voice: Voice {
                            style: StyleId(settings.time_signal_style),
                            ..Voice::default()
                        },
                        file: None,
                        origin: None,
                    },
                )
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jst_is_nine_hours_ahead_of_utc() {
        // 2026-01-01T00:00:00Z → JST 09:00。
        assert_eq!(jst_hour_minute(1_767_225_600), (9, 0));
    }

    #[test]
    fn minute_wraps_around_midnight() {
        // UTC 15:00 = JST 翌日 00:00。
        assert_eq!(jst_hour_minute(1_767_279_600), (0, 0));
    }

    #[test]
    fn hourly_boundary_is_only_the_top_of_the_hour() {
        assert!(is_boundary(0, 60));
        assert!(!is_boundary(30, 60));
        assert!(!is_boundary(1, 60));
    }

    #[test]
    fn half_hourly_boundary_matches_zero_and_thirty() {
        assert!(is_boundary(0, 30));
        assert!(is_boundary(30, 30));
        assert!(!is_boundary(15, 30));
    }

    #[test]
    fn announcement_omits_minutes_on_the_hour() {
        assert_eq!(announcement(14, 0), "ただいま14時です");
        assert_eq!(announcement(14, 30), "ただいま14時30分です");
    }
}
