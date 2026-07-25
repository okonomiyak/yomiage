mod commands;

use anyhow::Context as _;
use poise::serenity_prelude as serenity;
use songbird::SerenityInit;
use tracing_subscriber::EnvFilter;

/// アプリ層のエラーは anyhow に寄せる（PLAN §0）。
pub type Error = anyhow::Error;
pub type Context<'a> = poise::Context<'a, Data, Error>;

/// コマンド間で共有する状態。フェーズ 2 以降でキューや ENGINE クライアントが入る。
pub struct Data {}

/// コマンドが失敗しても Bot 全体は落とさない。ログに残し、実行者にだけ知らせる。
async fn on_error(error: poise::FrameworkError<'_, Data, Error>) {
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // .env が無くても環境変数から読めればよい（Docker では compose が渡す）。
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,yomiage_bot=debug")),
        )
        .init();

    // 起動時の設定読み込みだけは失敗即終了でよい（PLAN §0）。
    let token = std::env::var("DISCORD_TOKEN").context("DISCORD_TOKEN が設定されていない")?;

    // MESSAGE_CONTENT は特権インテント。Developer Portal で有効化しないと本文が空で届く（PLAN §12）。
    let intents = serenity::GatewayIntents::GUILDS
        | serenity::GatewayIntents::GUILD_VOICE_STATES
        | serenity::GatewayIntents::GUILD_MESSAGES
        | serenity::GatewayIntents::MESSAGE_CONTENT;

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: commands::all(),
            on_error: |error| Box::pin(on_error(error)),
            ..Default::default()
        })
        .setup(|ctx, ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                tracing::info!(user = %ready.user.name, "logged in");
                Ok(Data {})
            })
        })
        .build();

    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .register_songbird()
        .await
        .context("Discord クライアントの生成に失敗")?;

    client.start().await.context("Discord クライアントが停止")?;

    Ok(())
}
