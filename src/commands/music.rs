//! 音楽再生のコマンド。読み上げに被せて流す。キューは songbird に任せている。

use anyhow::anyhow;
use poise::serenity_prelude as serenity;

use crate::commands::playlist::autocomplete_playlist_name;
use crate::{Context, Error};

/// 音量の指定範囲（%）。100 を超えると歪むので上限にする。
const VOLUME_RANGE: std::ops::RangeInclusive<u32> = 0..=100;

/// 一覧で出す最大件数。
const QUEUE_LIMIT: usize = 20;

/// URL か検索語で音楽をキューに積む。何も流れていなければすぐ再生する。
#[poise::command(slash_command, guild_only)]
pub async fn play(
    ctx: Context<'_>,
    #[description = "URL・検索語、または /playlist に登録した名前"]
    #[autocomplete = "autocomplete_playlist_name"]
    query: String,
) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only コマンドなのに guild_id が取れない"))?;

    if !enabled(ctx, guild_id).await {
        ctx.say("音楽機能は無効です。`/feature` で有効にできます。")
            .await?;
        return Ok(());
    }

    let query = query.trim();
    if query.is_empty() {
        ctx.say("URL か検索語を指定してください。").await?;
        return Ok(());
    }

    // 登録名と完全一致したら、その URL を使う。無ければ今まで通り URL/検索語として扱う。
    let resolved = ctx.data().db.playlist_url(guild_id, query).await?;
    let query = resolved.as_deref().unwrap_or(query);

    // yt-dlp の起動と検索で数秒かかる。3 秒以内に返せないと Discord 側が失敗扱いにする。
    ctx.defer().await?;

    let volume = ctx
        .data()
        .db
        .guild_settings(guild_id)
        .await
        .map_or(0.3, |settings| settings.music_volume);

    match ctx.data().music.enqueue(guild_id, query, volume).await {
        Ok(queued) => {
            let percent = (volume * 100.0).round() as u32;
            let embed = if queued.position <= 1 {
                serenity::CreateEmbed::new()
                    .title("▶ 再生します")
                    .description(queued.track.link())
                    .color(serenity::Colour::BLURPLE)
                    .field("音量", format!("{percent}%"), true)
            } else {
                serenity::CreateEmbed::new()
                    .title("＋ キューに追加しました")
                    .description(queued.track.link())
                    .color(serenity::Colour::BLURPLE)
                    .field("順番", format!("{} 番目", queued.position), true)
            };
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
        }
        Err(error) => {
            tracing::warn!(%guild_id, %error, "failed to queue music");
            ctx.say(format!("再生できませんでした: {error}")).await?;
        }
    }
    Ok(())
}

/// 再生中の曲と、待っている曲を表示する。
#[poise::command(slash_command, guild_only)]
pub async fn queue(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only コマンドなのに guild_id が取れない"))?;

    let tracks = ctx.data().music.queue(guild_id).await;
    let Some((current, waiting)) = tracks.split_first() else {
        ctx.say("キューは空です。").await?;
        return Ok(());
    };

    let mut body = format!("▶ **{}**（再生中）", current.link());
    // 位置が取れないとき（曲の入れ替わりの瞬間など）はバーを省く。
    if let Some(now) = ctx.data().music.now_playing(guild_id).await {
        body.push('\n');
        body.push_str(&crate::music::progress_bar(now.position, now.duration));
    }
    for (index, track) in waiting.iter().take(QUEUE_LIMIT).enumerate() {
        body.push_str(&format!("\n{}. {}", index + 2, track.link()));
    }
    if waiting.len() > QUEUE_LIMIT {
        body.push_str(&format!("\n…ほか {} 件", waiting.len() - QUEUE_LIMIT));
    }

    let embed = serenity::CreateEmbed::new()
        .title("🎵 キュー")
        .description(body)
        .color(serenity::Colour::BLURPLE);
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

/// 今の曲を飛ばして次へ。
#[poise::command(slash_command, guild_only)]
pub async fn next(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only コマンドなのに guild_id が取れない"))?;

    if ctx.data().music.skip(guild_id).await {
        ctx.say("次の曲へ進みました。").await?;
    } else {
        ctx.say("音楽は流れていません。").await?;
    }
    Ok(())
}

/// 音楽を止めてキューを空にする（読み上げは止めない）。
#[poise::command(slash_command, guild_only)]
pub async fn stop(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only コマンドなのに guild_id が取れない"))?;

    if ctx.data().music.stop(guild_id).await {
        ctx.say("音楽を止めて、キューを空にしました。").await?;
    } else {
        ctx.say("音楽は流れていません。").await?;
    }
    Ok(())
}

/// 音楽の音量（サーバー単位。次に流す曲にも引き継がれる）。
#[poise::command(slash_command, guild_only)]
pub async fn volume(
    ctx: Context<'_>,
    #[description = "0〜100%（既定 30）"]
    #[min = 0]
    #[max = 100]
    percent: u32,
) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only コマンドなのに guild_id が取れない"))?;
    if !VOLUME_RANGE.contains(&percent) {
        ctx.say("0 〜 100 の範囲で指定してください。").await?;
        return Ok(());
    }

    let volume = percent as f32 / 100.0;
    ctx.data().db.set_music_volume(guild_id, volume).await?;
    // 再生中だけでなく待機中の曲にも当てておく。
    let applied = ctx.data().music.set_volume(guild_id, volume).await;

    tracing::info!(%guild_id, percent, applied, "music volume changed");
    let note = if applied > 0 {
        String::new()
    } else {
        "（次に流す曲から反映されます）".to_owned()
    };
    ctx.say(format!("音量を {percent}% にしました。{note}"))
        .await?;
    Ok(())
}

/// このサーバーで音楽機能が有効か。設定を読めないときは有効扱いにする。
pub async fn enabled(ctx: Context<'_>, guild_id: poise::serenity_prelude::GuildId) -> bool {
    ctx.data()
        .db
        .guild_settings(guild_id)
        .await
        .is_ok_and(|settings| settings.music_enabled)
}
