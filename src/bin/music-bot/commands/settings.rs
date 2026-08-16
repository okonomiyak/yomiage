//! 音楽 Bot のサーバー設定。読み上げ・時報の on/off は読み上げ Bot 側にある（PLAN §13）。

use anyhow::anyhow;

use crate::{Context, Error};

/// 音楽機能を有効・無効にする（サーバー単位）。
#[poise::command(slash_command, guild_only)]
pub async fn feature(
    ctx: Context<'_>,
    #[description = "有効にするなら true"] enabled: bool,
) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only コマンドなのに guild_id が取れない"))?;
    let data = ctx.data();

    data.db.set_music_enabled(guild_id, enabled).await?;
    if !enabled {
        // 溜まっているキューも捨てる。切った直後に鳴り続けると驚くので。
        data.music.stop(guild_id).await;
    }

    tracing::info!(%guild_id, enabled, "music feature toggled");
    let state = if enabled { "有効" } else { "無効" };
    ctx.say(format!("**音楽**を{state}にしました。")).await?;
    Ok(())
}
