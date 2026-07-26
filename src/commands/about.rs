//! クレジット表記（PLAN §12）。VOICEVOX の利用規約上、表記は必須。

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

    ctx.say(format!(
        "**yomiage-bot** — Discord のテキストチャンネルを読み上げる Bot\n\
         \n\
         音声合成: **VOICEVOX**\n\
         <https://voicevox.hiroshiba.jp/>\n\
         \n\
         あなたが使用中の音声ライブラリ: **{style}**\n\
         生成された音声を公開・配布する場合は、**「VOICEVOX:（キャラクター名）」**\
         のクレジット表記と、各キャラクターの利用規約の確認が必要です。\n\
         <https://voicevox.hiroshiba.jp/term/>\n\
         \n\
         ソース: <https://github.com/okonomiyak/yomiage>"
    ))
    .await?;
    Ok(())
}
