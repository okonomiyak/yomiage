//! 時報のサーバー設定（頻度・話者）。on/off は `/feature` から切り替える。
//! 時刻の読み上げそのものは `src/timesignal.rs` の背景タスクが行う。

use anyhow::anyhow;

use yomiage_bot::voicevox::StyleId;

use crate::commands::settings::{autocomplete_style, style_label};
use crate::{Context, Error};

/// 選べる頻度。
#[derive(Debug, poise::ChoiceParameter)]
pub enum Frequency {
    #[name = "毎正時（1時間おき）"]
    Hourly,
    #[name = "30分おき"]
    HalfHourly,
}

impl Frequency {
    fn minutes(&self) -> u32 {
        match self {
            Frequency::Hourly => 60,
            Frequency::HalfHourly => 30,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Frequency::Hourly => "毎正時",
            Frequency::HalfHourly => "30分おき",
        }
    }
}

/// 時報の設定（頻度・話者）。
#[poise::command(
    slash_command,
    guild_only,
    subcommands("interval", "voice"),
    subcommand_required
)]
pub async fn timesignal(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// 鳴らす頻度を設定する。
#[poise::command(slash_command, rename = "interval")]
pub async fn interval(
    ctx: Context<'_>,
    #[description = "頻度"] frequency: Frequency,
) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only なのに guild_id が取れない"))?;

    ctx.data()
        .db
        .set_time_signal_interval(guild_id, frequency.minutes())
        .await?;

    tracing::info!(%guild_id, minutes = frequency.minutes(), "time signal interval changed");
    ctx.say(format!(
        "時報の頻度を **{}** にしました。",
        frequency.label()
    ))
    .await?;
    Ok(())
}

/// 時報を読む話者を設定する（サーバー単位。発言者がいないイベントなので個人の声とは別）。
#[poise::command(slash_command, rename = "voice")]
pub async fn voice(
    ctx: Context<'_>,
    #[description = "話者（入力すると候補が絞られます）"]
    #[autocomplete = "autocomplete_style"]
    speaker: u32,
) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only なのに guild_id が取れない"))?;

    let styles = &ctx.data().styles;
    if !styles.is_empty() && !styles.iter().any(|choice| choice.id == speaker) {
        ctx.say("その話者は見つかりませんでした。候補から選んでください。")
            .await?;
        return Ok(());
    }

    ctx.data()
        .db
        .set_time_signal_style(guild_id, StyleId(speaker))
        .await?;

    let label = style_label(ctx, speaker);
    tracing::info!(%guild_id, speaker, "time signal voice changed");
    ctx.say(format!("時報の話者を **{label}** にしました。"))
        .await?;
    Ok(())
}
