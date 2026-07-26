//! 音楽の操作パネル。
//!
//! ボタンの押下は毎回コレクタで待つのではなく、`InteractionCreate` を
//! `custom_id` で振り分けて処理する。こうしておくと Bot を再起動しても、
//! 前に貼ったパネルがそのまま使える。

use anyhow::anyhow;
use poise::serenity_prelude as serenity;

use crate::{Context, Data, Error};

/// このパネルのボタンだと分かるようにする接頭辞。
const PREFIX: &str = "music:";

/// 一覧に出す待機曲の数。パネルなので短めにする。
const QUEUE_PREVIEW: usize = 5;

/// 音楽の操作パネルを出す。
#[poise::command(slash_command, guild_only)]
pub async fn dashboard(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only コマンドなのに guild_id が取れない"))?;

    if !super::music::enabled(ctx, guild_id).await {
        ctx.say("音楽機能は無効です。`/feature` で有効にできます。")
            .await?;
        return Ok(());
    }

    let (content, components) = build(ctx.data(), guild_id).await;
    ctx.send(
        poise::CreateReply::default()
            .content(content)
            .components(components),
    )
    .await?;
    Ok(())
}

/// パネルのボタンが押されたときの処理。押されていないものは無視する。
pub async fn handle_component(
    ctx: &serenity::Context,
    interaction: &serenity::ComponentInteraction,
    data: &Data,
) {
    let Some(action) = interaction.data.custom_id.strip_prefix(PREFIX) else {
        return;
    };
    let Some(guild_id) = interaction.guild_id else {
        return;
    };

    match action {
        "toggle" => {
            data.music.toggle_pause(guild_id).await;
        }
        "next" => {
            data.music.skip(guild_id).await;
        }
        "stop" => {
            data.music.stop(guild_id).await;
        }
        // 押しただけで最新の状態に描き替わる。
        "refresh" => {}
        other => {
            tracing::debug!(action = other, "unknown dashboard action");
            return;
        }
    }
    tracing::info!(%guild_id, action, user = %interaction.user.id, "dashboard used");

    let (content, components) = build(data, guild_id).await;
    let response = serenity::CreateInteractionResponse::UpdateMessage(
        serenity::CreateInteractionResponseMessage::new()
            .content(content)
            .components(components),
    );
    if let Err(error) = interaction.create_response(&ctx.http, response).await {
        tracing::warn!(%guild_id, %error, "failed to update dashboard");
    }
}

/// 今の状態から本文とボタンを作る。押されるたびに作り直す。
async fn build(
    data: &Data,
    guild_id: serenity::GuildId,
) -> (String, Vec<serenity::CreateActionRow>) {
    let titles = data.music.queue(guild_id).await;
    let paused = data.music.is_paused(guild_id).await;
    let volume = data
        .db
        .guild_settings(guild_id)
        .await
        .map_or(0.3, |settings| settings.music_volume);
    let percent = (volume * 100.0).round() as u32;

    let content = match titles.split_first() {
        None => "**音楽コントロール**\n再生していません。`/play` で追加してください。".to_owned(),
        Some((current, waiting)) => {
            let state = if paused {
                "⏸ 一時停止中"
            } else {
                "▶ 再生中"
            };
            let mut body =
                format!("**音楽コントロール**（音量 {percent}%）\n{state}: **{current}**");
            for (index, title) in waiting.iter().take(QUEUE_PREVIEW).enumerate() {
                body.push_str(&format!("\n{}. {title}", index + 2));
            }
            if waiting.len() > QUEUE_PREVIEW {
                body.push_str(&format!("\n…ほか {} 件", waiting.len() - QUEUE_PREVIEW));
            }
            body
        }
    };

    let row = serenity::CreateActionRow::Buttons(vec![
        serenity::CreateButton::new(format!("{PREFIX}toggle"))
            .emoji(if paused { '▶' } else { '⏸' })
            .label(if paused { "再開" } else { "一時停止" })
            .style(serenity::ButtonStyle::Primary),
        serenity::CreateButton::new(format!("{PREFIX}next"))
            .emoji('⏭')
            .label("次へ")
            .style(serenity::ButtonStyle::Secondary),
        serenity::CreateButton::new(format!("{PREFIX}stop"))
            .emoji('⏹')
            .label("停止")
            .style(serenity::ButtonStyle::Danger),
        serenity::CreateButton::new(format!("{PREFIX}refresh"))
            .emoji('🔄')
            .label("更新")
            .style(serenity::ButtonStyle::Secondary),
    ]);

    (content, vec![row])
}
