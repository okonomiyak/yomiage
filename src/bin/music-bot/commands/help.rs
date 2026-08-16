//! 使い方の表示。返信は ephemeral（実行者にだけ見える）にして、
//! チャンネルを長文で流さないようにする。

use poise::serenity_prelude as serenity;

use crate::{Context, Error};

/// 使い方を表示する（自分にだけ見えます）。
#[poise::command(slash_command)]
pub async fn help(ctx: Context<'_>) -> Result<(), Error> {
    let embed = serenity::CreateEmbed::new()
        .title("使い方")
        .color(serenity::Colour::BLURPLE)
        .field(
            "音楽",
            "\
`/join` … 参加中の VC に接続する / `/leave` … 切断してキューを空にする
`/play <URL または検索語>` … キューに積む。空いていればすぐ流す
　（YouTube とニコニコ動画の URL に対応。検索語のときは YouTube から探す）
　（YouTube の再生リスト・ニコニコのマイリスト/シリーズの URL は、まとめて最大 50 曲キューに積む）
`/queue` … 再生中と待機中を見る
`/next` … 次の曲へ / `/stop` … 止めてキューを空にする
`/volume <0-100>` … 音量（サーバー単位）
`/dashboard` … シークバー付きの操作パネルを出す。⏪ ⏩ で 10 秒／60 秒ずつ動かせる
　（バーは 5 秒ごとに自動で進む。しばらく何も流れないと止まるが、ボタンを押せば再開する）
`/playlist add <名前> <URL>` … よく聴く URL を名前で登録（サーバー共有）
`/playlist list` / `/playlist remove` … お気に入りの確認・削除
　（登録した名前は `/play <名前>` でそのまま呼び出せる）
`/feature <有効/無効>` … 音楽機能を on/off",
            false,
        )
        .field(
            "覚えておくと楽なこと",
            "\
・VC から全員いなくなると自動で退出する
・Bot を再起動すると VC から抜けるので `/join` からやり直す
・読み上げ Bot とは別の Bot なので、読み上げの設定は読み上げ Bot 側の `/help` を見る",
            false,
        );

    ctx.send(poise::CreateReply::default().embed(embed).ephemeral(true))
        .await?;
    Ok(())
}
