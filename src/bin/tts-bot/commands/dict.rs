//! サーバー辞書（PLAN §3 / §7-4）。読みはテキスト置換で当てる。

use anyhow::Context as _;
use anyhow::anyhow;
use poise::serenity_prelude as serenity;

use crate::{Context, Error};

/// 一覧で出す最大件数。Discord のメッセージ長に収まる範囲に抑える。
const LIST_LIMIT: usize = 50;

/// サーバー辞書の管理。
#[poise::command(
    slash_command,
    guild_only,
    subcommands("add", "list", "remove", "upload", "export"),
    subcommand_required
)]
pub async fn dict(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// 読みを登録する。
#[poise::command(slash_command)]
pub async fn add(
    ctx: Context<'_>,
    #[description = "表記（メッセージ中のこの文字列を置き換えます）"] surface: String,
    #[description = "読み"] reading: String,
) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only なのに guild_id が取れない"))?;

    let surface = surface.trim();
    let reading = reading.trim();
    if surface.is_empty() || reading.is_empty() {
        ctx.say("表記と読みの両方を指定してください。").await?;
        return Ok(());
    }

    ctx.data()
        .db
        .add_dictionary_entry(guild_id, surface, reading)
        .await?;

    tracing::info!(%guild_id, surface, reading, "dictionary entry added");
    let embed = serenity::CreateEmbed::new()
        .title("辞書に登録しました")
        .color(serenity::Colour::BLURPLE)
        .field("表記", surface, true)
        .field("読み", reading, true);
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

/// 登録済みの読みを一覧表示する。
#[poise::command(slash_command)]
pub async fn list(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only なのに guild_id が取れない"))?;

    let entries = ctx.data().db.dictionary(guild_id).await?;
    if entries.is_empty() {
        ctx.say("辞書は空です。").await?;
        return Ok(());
    }

    let total = entries.len();
    let body = entries
        .iter()
        .take(LIST_LIMIT)
        .map(|(surface, reading)| format!("・{surface} → {reading}"))
        .collect::<Vec<_>>()
        .join("\n");

    let note = if total > LIST_LIMIT {
        format!("\n（{total} 件中 {LIST_LIMIT} 件を表示）")
    } else {
        String::new()
    };

    ctx.say(format!("**サーバー辞書**\n{body}{note}")).await?;
    Ok(())
}

/// 登録を削除する。
#[poise::command(slash_command)]
pub async fn remove(
    ctx: Context<'_>,
    #[description = "削除する表記"] surface: String,
) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only なのに guild_id が取れない"))?;

    let surface = surface.trim();
    if ctx
        .data()
        .db
        .remove_dictionary_entry(guild_id, surface)
        .await?
    {
        tracing::info!(%guild_id, surface, "dictionary entry removed");
        ctx.say(format!("削除しました: **{surface}**")).await?;
    } else {
        ctx.say(format!("**{surface}** は登録されていません。"))
            .await?;
    }
    Ok(())
}

/// CSV（表記,読み。1行1件）ファイルからまとめて登録する。
/// 既存の表記と重なる行は読みを上書きし、それ以外の既存エントリはそのまま残る。
#[poise::command(slash_command)]
pub async fn upload(
    ctx: Context<'_>,
    #[description = "CSV ファイル（1行 = 表記,読み）"] file: serenity::Attachment,
) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only なのに guild_id が取れない"))?;

    ctx.defer().await?;

    let bytes = file
        .download()
        .await
        .context("添付ファイルのダウンロードに失敗しました")?;
    let text = String::from_utf8_lossy(&bytes);

    let mut added = 0usize;
    let mut skipped = 0usize;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((surface, reading)) = line.split_once(',') else {
            skipped += 1;
            continue;
        };
        let surface = surface.trim();
        let reading = reading.trim();
        if surface.is_empty() || reading.is_empty() {
            skipped += 1;
            continue;
        }
        match ctx
            .data()
            .db
            .add_dictionary_entry(guild_id, surface, reading)
            .await
        {
            Ok(()) => added += 1,
            Err(error) => {
                tracing::warn!(%guild_id, surface, %error, "failed to add dictionary entry from upload");
                skipped += 1;
            }
        }
    }

    tracing::info!(%guild_id, added, skipped, "dictionary uploaded");
    let embed = serenity::CreateEmbed::new()
        .title("辞書をアップロードしました")
        .color(serenity::Colour::BLURPLE)
        .field("登録", format!("{added} 件"), true)
        .field("スキップ", format!("{skipped} 件"), true);
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

/// 登録済みの辞書を CSV ファイルとして出力する（`/dict upload` と同じ形式）。
#[poise::command(slash_command)]
pub async fn export(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only なのに guild_id が取れない"))?;

    let entries = ctx.data().db.dictionary(guild_id).await?;
    if entries.is_empty() {
        ctx.say("辞書は空です。").await?;
        return Ok(());
    }

    let csv = entries
        .iter()
        .map(|(surface, reading)| format!("{surface},{reading}"))
        .collect::<Vec<_>>()
        .join("\n");

    let attachment = serenity::CreateAttachment::bytes(csv.into_bytes(), "dictionary.csv");
    ctx.send(poise::CreateReply::default().attachment(attachment))
        .await?;
    Ok(())
}
