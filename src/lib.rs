//! 読み上げ Bot（`tts-bot`）と音楽 Bot（`music-bot`）が共有するロジック（PLAN §13）。
//!
//! それぞれ別プロセス・別 Discord アプリとして動く。共有するのは DB スキーマと
//! 読み上げ／音楽それぞれのマネージャー本体で、コマンド定義・`Data`・イベント
//! ハンドラは各バイナリ（`src/bin/tts-bot` / `src/bin/music-bot`）が個別に持つ。

pub mod db;
pub mod exvoice;
pub mod music;
pub mod nicovideo;
pub mod session;
pub mod speech;
pub mod text;
pub mod timesignal;
pub mod voicevox;

use poise::serenity_prelude::{self as serenity};

/// アプリ層のエラーは anyhow に寄せる（PLAN §0）。
pub type Error = anyhow::Error;

/// コマンドが失敗しても Bot 全体は落とさない。ログに残し、実行者にだけ知らせる。
/// `tts-bot` / `music-bot` の両方から使う（`Data` の型が違うのでジェネリックにする）。
pub async fn on_error<D>(error: poise::FrameworkError<'_, D, Error>) {
    match error {
        poise::FrameworkError::Setup { error, .. } => {
            tracing::error!(?error, "framework setup failed");
        }
        poise::FrameworkError::Command { error, ctx, .. } => {
            let command = ctx.command().qualified_name.clone();
            tracing::error!(command, ?error, "command failed");
            if let Err(e) = ctx.say("コマンドの実行に失敗しました。").await {
                tracing::warn!(?e, "failed to report command error to user");
            }
        }
        error => {
            if let Err(e) = poise::builtins::on_error(error).await {
                tracing::error!(?e, "error while handling error");
            }
        }
    }
}

/// スラッシュコマンドを登録する。
///
/// `GUILD_ID` があればそのサーバーだけに登録する。ギルド登録は**即時反映**なので開発中はこちら。
/// 空ならグローバル登録で、こちらは反映に最大 1 時間かかる。
pub async fn register_commands<D>(
    ctx: &serenity::Context,
    options: &poise::FrameworkOptions<D, Error>,
    guilds: &[serenity::UnavailableGuild],
) -> anyhow::Result<()> {
    let guild_id = std::env::var("GUILD_ID")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .map(serenity::GuildId::new);

    let Some(guild_id) = guild_id else {
        poise::builtins::register_globally(ctx, &options.commands).await?;
        // ギルド限定で登録したものが残っていると、そのサーバーだけ一覧に二重で出る。
        // Bot が居る全ギルドを空で上書きして消す。
        for guild in guilds {
            if let Err(error) =
                poise::builtins::register_in_guild(ctx, &options.commands[..0], guild.id).await
            {
                tracing::warn!(guild_id = %guild.id, %error, "failed to clear guild commands");
            }
        }
        tracing::info!(
            count = options.commands.len(),
            guilds = guilds.len(),
            "commands registered globally",
        );
        return Ok(());
    };

    poise::builtins::register_in_guild(ctx, &options.commands, guild_id).await?;
    // 以前グローバルに登録したものが残っていると一覧に二重で出る。空で上書きして消す。
    poise::builtins::register_globally(ctx, &options.commands[..0]).await?;
    tracing::info!(
        %guild_id,
        count = options.commands.len(),
        "commands registered for a single guild (instant)",
    );
    Ok(())
}

/// SIGTERM か Ctrl-C を待つ。SIGTERM は Unix のみ。
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = term.recv() => {},
                    _ = tokio::signal::ctrl_c() => {},
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to listen for SIGTERM; falling back to Ctrl-C");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
