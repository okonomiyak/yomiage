//! 個人設定コマンド（PLAN §3）。設定はユーザー単位でギルド横断（§13-2）。

use poise::serenity_prelude as serenity;

use crate::voicevox::{INTONATION_RANGE, PITCH_RANGE, SPEED_RANGE, StyleId};
use crate::{Context, Error};

/// Discord のオートコンプリートは 25 件まで。
const AUTOCOMPLETE_LIMIT: usize = 25;

/// 起動時に `/speakers` から作るオートコンプリート用の候補。
#[derive(Debug, Clone)]
pub struct StyleChoice {
    pub label: String,
    pub id: u32,
}

async fn autocomplete_style<'a>(
    ctx: Context<'a>,
    partial: &'a str,
) -> impl Iterator<Item = serenity::AutocompleteChoice> + 'a {
    let needle = partial.to_lowercase();
    ctx.data()
        .styles
        .iter()
        .filter(move |choice| choice.label.to_lowercase().contains(&needle))
        .take(AUTOCOMPLETE_LIMIT)
        .map(|choice| serenity::AutocompleteChoice::new(choice.label.clone(), choice.id))
}

/// 自分の話者を設定する。
#[poise::command(slash_command)]
pub async fn voice(
    ctx: Context<'_>,
    #[description = "話者（入力すると候補が絞られます）"]
    #[autocomplete = "autocomplete_style"]
    speaker: u32,
) -> Result<(), Error> {
    let styles = &ctx.data().styles;
    // ENGINE が落ちていて一覧を取れていないときは検証を諦めてそのまま通す。
    if !styles.is_empty() && !styles.iter().any(|choice| choice.id == speaker) {
        ctx.say("その話者は見つかりませんでした。候補から選んでください。")
            .await?;
        return Ok(());
    }

    ctx.data()
        .db
        .set_style(ctx.author().id, StyleId(speaker))
        .await?;

    let label = style_label(ctx, speaker);
    tracing::info!(user = %ctx.author().id, speaker, "voice changed");
    ctx.say(format!("話者を **{label}** にしました。")).await?;
    Ok(())
}

/// 話す速さ。
#[poise::command(slash_command)]
pub async fn speed(
    ctx: Context<'_>,
    #[description = "0.5〜2.0（既定 1.0）"]
    #[min = 0.5]
    #[max = 2.0]
    value: f32,
) -> Result<(), Error> {
    if !SPEED_RANGE.contains(&value) {
        ctx.say("0.5 〜 2.0 の範囲で指定してください。").await?;
        return Ok(());
    }
    ctx.data().db.set_speed(ctx.author().id, value).await?;
    ctx.say(format!("速さを {value} にしました。")).await?;
    Ok(())
}

/// 声の高さ。
#[poise::command(slash_command)]
pub async fn pitch(
    ctx: Context<'_>,
    #[description = "-0.15〜0.15（既定 0.0）"]
    #[min = -0.15]
    #[max = 0.15]
    value: f32,
) -> Result<(), Error> {
    if !PITCH_RANGE.contains(&value) {
        ctx.say("-0.15 〜 0.15 の範囲で指定してください。").await?;
        return Ok(());
    }
    ctx.data().db.set_pitch(ctx.author().id, value).await?;
    ctx.say(format!("高さを {value} にしました。")).await?;
    Ok(())
}

/// 抑揚。
#[poise::command(slash_command)]
pub async fn intonation(
    ctx: Context<'_>,
    #[description = "0.0〜2.0（既定 1.0）"]
    #[min = 0.0]
    #[max = 2.0]
    value: f32,
) -> Result<(), Error> {
    if !INTONATION_RANGE.contains(&value) {
        ctx.say("0.0 〜 2.0 の範囲で指定してください。").await?;
        return Ok(());
    }
    ctx.data().db.set_intonation(ctx.author().id, value).await?;
    ctx.say(format!("抑揚を {value} にしました。")).await?;
    Ok(())
}

/// このサーバーの設定と自分の声を表示する。
#[poise::command(slash_command, guild_only)]
pub async fn config(ctx: Context<'_>) -> Result<(), Error> {
    let Some(guild_id) = ctx.guild_id() else {
        ctx.say("サーバー内で実行してください。").await?;
        return Ok(());
    };
    let data = ctx.data();

    let channels = data.db.read_channels(guild_id).await?;
    let settings = data.db.guild_settings(guild_id).await?;
    let voice = data.db.voice(ctx.author().id).await?;

    let channel_list = if channels.is_empty() {
        "（未登録）".to_owned()
    } else {
        channels
            .iter()
            .map(|id| format!("<#{id}>"))
            .collect::<Vec<_>>()
            .join(" ")
    };

    ctx.say(format!(
        "**サーバー設定**\n\
         読み上げ対象: {channel_list}\n\
         文字数上限: {} 文字\n\
         Bot の発言: {}\n\
         無視する接頭辞: `{}`\n\
         \n\
         **あなたの声**（サーバー共通）\n\
         話者: {} (ID {})\n\
         速さ: {} / 高さ: {} / 抑揚: {}",
        settings.max_length,
        if settings.read_bots {
            "読む"
        } else {
            "読まない"
        },
        settings.ignore_prefix,
        style_label(ctx, voice.style.0),
        voice.style,
        voice.speed,
        voice.pitch,
        voice.intonation,
    ))
    .await?;
    Ok(())
}

fn style_label(ctx: Context<'_>, id: u32) -> String {
    ctx.data()
        .styles
        .iter()
        .find(|choice| choice.id == id)
        .map_or_else(|| format!("ID {id}"), |choice| choice.label.clone())
}
