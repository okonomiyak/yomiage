//! お気に入り音楽（サーバー共有）。登録した名前は `/play <名前>` からも呼べる。

use anyhow::anyhow;
use poise::serenity_prelude as serenity;

use crate::{Context, Error};

/// 一覧で出す最大件数。
const LIST_LIMIT: usize = 50;

/// Discord のオートコンプリートは 25 件まで。
const AUTOCOMPLETE_LIMIT: usize = 25;

/// よく聴く URL を名前で登録し、`/play <名前>` で呼び出せるようにする。
#[poise::command(
    slash_command,
    guild_only,
    subcommands("add", "list", "remove"),
    subcommand_required
)]
pub async fn playlist(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// URL を名前で登録する。
#[poise::command(slash_command)]
pub async fn add(
    ctx: Context<'_>,
    #[description = "呼び出し用の名前"] name: String,
    #[description = "URL"] url: String,
) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only なのに guild_id が取れない"))?;

    let name = name.trim();
    let url = url.trim();
    if name.is_empty() || url.is_empty() {
        ctx.say("名前と URL の両方を指定してください。").await?;
        return Ok(());
    }

    ctx.data()
        .db
        .add_playlist_entry(guild_id, name, url)
        .await?;

    tracing::info!(%guild_id, name, "playlist entry added");
    ctx.say(format!("登録しました: **{name}** → {url}")).await?;
    Ok(())
}

/// 登録済みの一覧を表示する。
#[poise::command(slash_command)]
pub async fn list(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only なのに guild_id が取れない"))?;

    let entries = ctx.data().db.playlist(guild_id).await?;
    if entries.is_empty() {
        ctx.say("お気に入りは空です。`/playlist add` で登録できます。")
            .await?;
        return Ok(());
    }

    let total = entries.len();
    let body = entries
        .iter()
        .take(LIST_LIMIT)
        .map(|(name, url)| format!("・**{name}** → {url}"))
        .collect::<Vec<_>>()
        .join("\n");

    let note = if total > LIST_LIMIT {
        format!("\n（{total} 件中 {LIST_LIMIT} 件を表示）")
    } else {
        String::new()
    };

    let embed = serenity::CreateEmbed::new()
        .title("🎵 お気に入り")
        .description(format!("{body}{note}"))
        .color(serenity::Colour::BLURPLE);
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

/// 登録を削除する。
#[poise::command(slash_command)]
pub async fn remove(
    ctx: Context<'_>,
    #[description = "削除する名前"]
    #[autocomplete = "autocomplete_playlist_name"]
    name: String,
) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only なのに guild_id が取れない"))?;

    let name = name.trim();
    if ctx.data().db.remove_playlist_entry(guild_id, name).await? {
        tracing::info!(%guild_id, name, "playlist entry removed");
        ctx.say(format!("削除しました: **{name}**")).await?;
    } else {
        ctx.say(format!("**{name}** は登録されていません。"))
            .await?;
    }
    Ok(())
}

/// 登録済みの名前を候補に出す。`/playlist remove` と `/play` の両方から使う。
pub(crate) async fn autocomplete_playlist_name<'a>(
    ctx: Context<'a>,
    partial: &'a str,
) -> impl Iterator<Item = serenity::AutocompleteChoice> + 'a {
    let entries = match ctx.guild_id() {
        Some(guild_id) => ctx.data().db.playlist(guild_id).await.unwrap_or_default(),
        None => Vec::new(),
    };

    let needle = partial.trim().to_lowercase();
    entries
        .into_iter()
        .filter(move |(name, _)| needle.is_empty() || name.to_lowercase().contains(&needle))
        .take(AUTOCOMPLETE_LIMIT)
        .map(|(name, _)| serenity::AutocompleteChoice::new(name.clone(), name))
}
