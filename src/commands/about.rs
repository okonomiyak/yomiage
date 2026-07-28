//! クレジット表記（PLAN §12）。VOICEVOX の利用規約上、表記は必須。

use poise::serenity_prelude as serenity;

use crate::{Context, Error};

/// この Bot について（音声合成のクレジット）。
#[poise::command(slash_command)]
pub async fn about(ctx: Context<'_>) -> Result<(), Error> {
    // 自分の話者名も出す。キャラクターごとに利用規約が違うため。
    let style = match ctx.data().db.voice(ctx.author().id).await {
        Ok(voice) => ctx
            .data()
            .styles
            .iter()
            .find(|choice| choice.id == voice.style.0)
            .map_or_else(
                || format!("ID {}", voice.style),
                |choice| choice.label.clone(),
            ),
        Err(_) => "（取得できませんでした）".to_owned(),
    };

    let embed = serenity::CreateEmbed::new()
        .title("yomiage-bot")
        .description("Discord のテキストチャンネルを読み上げる Bot")
        .color(serenity::Colour::BLURPLE)
        .field(
            "音声合成",
            "**VOICEVOX**\n<https://voicevox.hiroshiba.jp/>",
            false,
        )
        .field("あなたが使用中の音声ライブラリ", style, true)
        .field(
            "クレジット表記について",
            "生成された音声を公開・配布する場合は、**「VOICEVOX:（キャラクター名）」**\
             のクレジット表記と、各キャラクターの利用規約の確認が必要です。\n\
             <https://voicevox.hiroshiba.jp/term/>",
            false,
        )
        .field("ソース", "<https://github.com/okonomiyak/yomiage>", false);

    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}
