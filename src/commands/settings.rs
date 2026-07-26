//! 個人設定コマンド（PLAN §3）。設定はユーザー単位でギルド横断（§13-2）。

use poise::serenity_prelude as serenity;

use crate::voicevox::{INTONATION_RANGE, PITCH_RANGE, SPEED_RANGE, StyleId};
use crate::{Context, Error};

/// Discord のオートコンプリートは 25 件まで。
const AUTOCOMPLETE_LIMIT: usize = 25;

/// 読み上げ文字数の上限に許す範囲。長すぎると 1 発言で数十秒占有してしまう。
const MAX_LENGTH_RANGE: std::ops::RangeInclusive<u32> = 1..=500;

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
    let needle = partial.trim().to_lowercase();

    // 日本語入力だと変換確定前の文字列がそのまま飛んでくるので、名前だけだと
    // 引っかからないことがある。ID でも探せるようにしておく。
    let mut matched: Vec<&StyleChoice> = ctx
        .data()
        .styles
        .iter()
        .filter(|choice| {
            needle.is_empty()
                || choice.label.to_lowercase().contains(&needle)
                || choice.id.to_string().starts_with(&needle)
        })
        .collect();

    // 25 件しか返せないので、前方一致を先に出す。名前を打ち込んだのに
    // 埋もれて出てこない、という状態を避ける。
    matched.sort_by_key(|choice| !choice.label.to_lowercase().starts_with(&needle));

    matched
        .into_iter()
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

/// 1 メッセージで読み上げる最大文字数（サーバー単位）。
#[poise::command(slash_command, guild_only)]
pub async fn maxlength(
    ctx: Context<'_>,
    #[description = "1〜500 文字（既定 100）。超えた分は「以下略」になります"]
    #[min = 1]
    #[max = 500]
    value: u32,
) -> Result<(), Error> {
    let Some(guild_id) = ctx.guild_id() else {
        ctx.say("サーバー内で実行してください。").await?;
        return Ok(());
    };
    if !MAX_LENGTH_RANGE.contains(&value) {
        ctx.say("1 〜 500 の範囲で指定してください。").await?;
        return Ok(());
    }

    ctx.data()
        .db
        .set_max_length(guild_id, value as usize)
        .await?;

    tracing::info!(%guild_id, value, "max length changed");
    // 長くすると合成に時間がかかる（実測で 90 文字 ≒ 4.6 秒 / PLAN §4.1）。
    let note = if value > 200 {
        "\n長い設定なので、上限に近い発言は読み始めるまで数秒かかります。"
    } else {
        ""
    };
    ctx.say(format!(
        "読み上げの文字数上限を {value} 文字にしました。{note}"
    ))
    .await?;
    Ok(())
}

/// 切り替えられる機能。
#[derive(Debug, poise::ChoiceParameter)]
pub enum Feature {
    #[name = "読み上げ"]
    Tts,
    #[name = "音楽"]
    Music,
}

/// 読み上げ／音楽を個別に有効・無効にする（サーバー単位）。
#[poise::command(slash_command, guild_only)]
pub async fn feature(
    ctx: Context<'_>,
    #[description = "切り替える機能"] feature: Feature,
    #[description = "有効にするなら true"] enabled: bool,
) -> Result<(), Error> {
    let Some(guild_id) = ctx.guild_id() else {
        ctx.say("サーバー内で実行してください。").await?;
        return Ok(());
    };
    let data = ctx.data();

    let name = match feature {
        Feature::Tts => {
            data.db.set_tts_enabled(guild_id, enabled).await?;
            if !enabled {
                // 溜まっている読み上げも捨てる。切った直後に喋り続けると驚くので。
                data.speech.stop(guild_id).await;
            }
            "読み上げ"
        }
        Feature::Music => {
            data.db.set_music_enabled(guild_id, enabled).await?;
            if !enabled {
                data.music.stop(guild_id).await;
            }
            "音楽"
        }
    };

    tracing::info!(%guild_id, name, enabled, "feature toggled");
    let state = if enabled { "有効" } else { "無効" };
    ctx.say(format!("**{name}**を{state}にしました。")).await?;
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
    let bindings = data.db.bindings(guild_id).await?;
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

    let binding_list = if bindings.is_empty() {
        "（なし）".to_owned()
    } else {
        bindings
            .iter()
            .map(|(text, voice)| format!("<#{text}> → <#{voice}>"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.say(format!(
        "**サーバー設定**\n\
         機能: 読み上げ {} / 音楽 {}\n\
         読み上げ中: {channel_list}\n\
         紐づけ:\n{binding_list}\n\
         文字数上限: {} 文字\n\
         Bot の発言: {}\n\
         無視する接頭辞: `{}`\n\
         \n\
         **あなたの声**（サーバー共通）\n\
         話者: {} (ID {})\n\
         速さ: {} / 高さ: {} / 抑揚: {}",
        if settings.tts_enabled {
            "有効"
        } else {
            "無効"
        },
        if settings.music_enabled {
            "有効"
        } else {
            "無効"
        },
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
