//! 音楽再生のコマンド。読み上げに被せて流す。

use anyhow::anyhow;

use crate::{Context, Error};

/// 音量の指定範囲（%）。100 を超えると歪むので上限にする。
const VOLUME_RANGE: std::ops::RangeInclusive<u32> = 0..=100;

/// URL か検索語で音楽を流す。
#[poise::command(slash_command, guild_only)]
pub async fn play(
    ctx: Context<'_>,
    #[description = "URL または検索語"] query: String,
) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only コマンドなのに guild_id が取れない"))?;

    let query = query.trim();
    if query.is_empty() {
        ctx.say("URL か検索語を指定してください。").await?;
        return Ok(());
    }

    // yt-dlp の起動と検索で数秒かかる。3 秒以内に返せないと Discord 側が失敗扱いにする。
    ctx.defer().await?;

    let volume = ctx
        .data()
        .db
        .guild_settings(guild_id)
        .await
        .map_or(0.3, |settings| settings.music_volume);

    match ctx.data().music.play(guild_id, query, volume).await {
        Ok(playing) => {
            let title = playing.title.unwrap_or_else(|| query.to_owned());
            let percent = (volume * 100.0).round() as u32;
            ctx.say(format!("再生します: **{title}**（音量 {percent}%）"))
                .await?;
        }
        Err(error) => {
            tracing::warn!(%guild_id, %error, "failed to start music");
            ctx.say(format!("再生できませんでした: {error}")).await?;
        }
    }
    Ok(())
}

/// 音楽を止める（読み上げは止めない）。
#[poise::command(slash_command, guild_only)]
pub async fn stop(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only コマンドなのに guild_id が取れない"))?;

    if ctx.data().music.stop(guild_id).await {
        ctx.say("音楽を止めました。").await?;
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
    let applied = ctx.data().music.set_volume(guild_id, volume).await;

    tracing::info!(%guild_id, percent, applied, "music volume changed");
    let note = if applied {
        ""
    } else {
        "（次に流す曲から反映されます）"
    };
    ctx.say(format!("音量を {percent}% にしました。{note}"))
        .await?;
    Ok(())
}
