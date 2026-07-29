//! 読み上げ統計（PLAN §13-11）。文字数のみをサーバー×ユーザー単位で数える。

use anyhow::anyhow;
use poise::serenity_prelude as serenity;

use crate::{Context, Error};

/// 一覧で出す最大件数。
const LIST_LIMIT: usize = 20;

/// このサーバーの読み上げ文字数を表示する（当日・累計、ユーザー別）。
#[poise::command(slash_command, guild_only)]
pub async fn stats(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only なのに guild_id が取れない"))?;

    let day = crate::timesignal::jst_day(crate::now_unix());
    let mut rows = ctx.data().db.speech_stats(guild_id, day).await?;
    if rows.is_empty() {
        ctx.say("まだ読み上げの記録がありません。").await?;
        return Ok(());
    }

    let today_total: i64 = rows.iter().map(|(_, today, _)| today).sum();
    let total: i64 = rows.iter().map(|(_, _, total)| total).sum();

    // 累計が多い順。よく読まれているユーザーを上に出す。
    rows.sort_by_key(|(_, _, total)| std::cmp::Reverse(*total));

    let ranked = rows.len();
    let body = rows
        .iter()
        .take(LIST_LIMIT)
        .map(|(user_id, today, total)| {
            format!("・<@{user_id}> 今日 {today} 文字／累計 {total} 文字")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let note = if ranked > LIST_LIMIT {
        format!("\n…ほか {} 人", ranked - LIST_LIMIT)
    } else {
        String::new()
    };

    let embed = serenity::CreateEmbed::new()
        .title("📊 読み上げ統計")
        .field(
            "サーバー合計",
            format!("今日 {today_total} 文字／累計 {total} 文字"),
            false,
        )
        .field("ユーザー別", format!("{body}{note}"), false)
        .color(serenity::Colour::BLURPLE);
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}
