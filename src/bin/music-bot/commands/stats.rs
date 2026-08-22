//! 音楽再生統計。曲別（タイトル単位）とユーザー別の再生回数を数える。
//! `/play` `/up_play` で実際に積んだ曲だけを数える（再生リストの一括登録は対象外）。

use anyhow::anyhow;
use poise::serenity_prelude as serenity;

use yomiage_bot::music::track_link;

use crate::{Context, Error};

/// 一覧で出す最大件数。
const LIST_LIMIT: usize = 10;

/// このサーバーの再生統計を表示する（曲別・ユーザー別、それぞれ再生回数順）。
#[poise::command(slash_command, guild_only)]
pub async fn stats(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only なのに guild_id が取れない"))?;

    let tracks = ctx.data().db.music_stats_by_track(guild_id).await?;
    if tracks.is_empty() {
        ctx.say("まだ再生の記録がありません。").await?;
        return Ok(());
    }
    let users = ctx.data().db.music_stats_by_user(guild_id).await?;

    let track_body = tracks
        .iter()
        .take(LIST_LIMIT)
        .map(|(title, plays)| format!("・{} 回 {}", plays, track_link(title, None)))
        .collect::<Vec<_>>()
        .join("\n");
    let user_body = users
        .iter()
        .take(LIST_LIMIT)
        .map(|(user_id, plays)| format!("・<@{user_id}> {plays} 回"))
        .collect::<Vec<_>>()
        .join("\n");

    let embed = serenity::CreateEmbed::new()
        .title("📊 再生統計")
        .field("曲別（再生回数順）", track_body, false)
        .field("ユーザー別（リクエスト回数順）", user_body, false)
        .color(serenity::Colour::BLURPLE);
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}
