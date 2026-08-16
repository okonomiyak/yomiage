//! 音楽 Bot の VC 接続。読み上げ Bot の `/join` と違い、テキストチャンネルの
//! 紐づけは無い（音楽は文章を読まないので不要）。実行者が今いる VC に繋ぐだけ。

use anyhow::anyhow;

use crate::{Context, Error};

/// 実行者が居るボイスチャンネルに接続する。
#[poise::command(slash_command, guild_only)]
pub async fn join(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only コマンドなのに guild_id が取れない"))?;

    // Guild の参照（キャッシュ）は Send でないため、必要な値だけ取り出してスコープを抜ける。
    let voice_channel = {
        let Some(guild) = ctx.guild() else {
            ctx.say("サーバー情報を取得できませんでした。").await?;
            return Ok(());
        };
        guild
            .voice_states
            .get(&ctx.author().id)
            .and_then(|state| state.channel_id)
    };

    let Some(voice_channel) = voice_channel else {
        ctx.say("先にボイスチャンネルに参加してください。").await?;
        return Ok(());
    };

    let manager = songbird::get(ctx.serenity_context())
        .await
        .ok_or_else(|| anyhow!("songbird が初期化されていない"))?;

    match manager.join(guild_id, voice_channel).await {
        Ok(_call) => {
            tracing::info!(%guild_id, %voice_channel, "joined voice channel");
            ctx.say(format!("<#{voice_channel}> に参加しました。"))
                .await?;
        }
        Err(error) => {
            tracing::warn!(%guild_id, %voice_channel, ?error, "failed to join voice channel");
            ctx.say("ボイスチャンネルへの接続に失敗しました。").await?;
        }
    }

    Ok(())
}

/// ボイスチャンネルから切断し、キューを空にする。
#[poise::command(slash_command, guild_only)]
pub async fn leave(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only コマンドなのに guild_id が取れない"))?;

    let manager = songbird::get(ctx.serenity_context())
        .await
        .ok_or_else(|| anyhow!("songbird が初期化されていない"))?;

    if manager.get(guild_id).is_none() {
        ctx.say("ボイスチャンネルに参加していません。").await?;
        return Ok(());
    }

    match manager.remove(guild_id).await {
        Ok(()) => {
            ctx.data().music.stop(guild_id).await;
            tracing::info!(%guild_id, "left voice channel");
            ctx.say("ボイスチャンネルから切断しました。").await?;
        }
        Err(error) => {
            tracing::warn!(%guild_id, ?error, "failed to leave voice channel");
            ctx.say("切断に失敗しました。").await?;
        }
    }

    Ok(())
}
