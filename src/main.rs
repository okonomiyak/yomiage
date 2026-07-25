mod commands;
mod speech;
mod voicevox;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context as _, anyhow};
use poise::serenity_prelude::{self as serenity, ChannelId, GuildId};
use songbird::SerenityInit;
use tokio::sync::RwLock;
use tracing_subscriber::EnvFilter;

use crate::speech::SpeechTask;

/// アプリ層のエラーは anyhow に寄せる（PLAN §0）。
pub type Error = anyhow::Error;
pub type Context<'a> = poise::Context<'a, Data, Error>;

/// 読み上げ文字数の上限（PLAN §4）。超過分の「以下略」はフェーズ 4 の正規化で入れる。
const MAX_LENGTH: usize = 100;
/// この接頭辞が付いた発言は読まない（PLAN §3）。フェーズ 3 でギルド設定にする。
const IGNORE_PREFIX: char = ';';

pub struct Data {
    pub speech: Arc<speech::Manager>,
    /// 読み上げ対象チャンネル。1 ギルドに複数登録できる（PLAN §13-3）。
    /// フェーズ 3 で SQLite に移すまではメモリのみ。
    pub read_channels: RwLock<HashMap<GuildId, HashSet<ChannelId>>>,
}

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

async fn event_handler(
    _ctx: &serenity::Context,
    event: &serenity::FullEvent,
    _framework: poise::FrameworkContext<'_, Data, Error>,
    data: &Data,
) -> Result<(), Error> {
    if let serenity::FullEvent::Message { new_message } = event {
        // 読み上げの失敗でイベント処理を落とさない。中でログに残して握る。
        handle_message(new_message, data).await;
    }
    Ok(())
}

/// メッセージを読み上げキューへ流す（PLAN §7 の 1〜2 と 6）。
/// テキスト正規化（§7-3）と辞書（§7-4）はフェーズ 4。
async fn handle_message(message: &serenity::Message, data: &Data) {
    let Some(guild_id) = message.guild_id else {
        return;
    };
    // 自分を含む Bot の発言は読まない。切替設定はフェーズ 3。
    if message.author.bot {
        return;
    }

    let is_target = {
        let channels = data.read_channels.read().await;
        channels
            .get(&guild_id)
            .is_some_and(|set| set.contains(&message.channel_id))
    };
    if !is_target {
        return;
    }

    let content = message.content.trim();
    if content.is_empty() || content.starts_with(IGNORE_PREFIX) {
        return;
    }
    let text: String = content.chars().take(MAX_LENGTH).collect();

    data.speech
        .enqueue(
            guild_id,
            SpeechTask {
                text,
                style: voicevox::DEFAULT_STYLE,
            },
        )
        .await;
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
    let voicevox_url =
        std::env::var("VOICEVOX_URL").unwrap_or_else(|_| "http://localhost:50021".to_owned());

    let engine = Arc::new(voicevox::Client::new(&voicevox_url).context("VOICEVOX_URL が不正")?);
    tracing::info!(url = voicevox_url, "voicevox engine configured");

    // MESSAGE_CONTENT は特権インテント。Developer Portal で有効化しないと本文が空で届く（PLAN §12）。
    let intents = serenity::GatewayIntents::GUILDS
        | serenity::GatewayIntents::GUILD_VOICE_STATES
        | serenity::GatewayIntents::GUILD_MESSAGES
        | serenity::GatewayIntents::MESSAGE_CONTENT;

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: commands::all(),
            on_error: |error| Box::pin(on_error(error)),
            event_handler: |ctx, event, framework, data| {
                Box::pin(event_handler(ctx, event, framework, data))
            },
            ..Default::default()
        })
        .setup(move |ctx, ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                tracing::info!(user = %ready.user.name, "logged in");

                warm_up(&engine).await;

                let songbird = songbird::get(ctx)
                    .await
                    .ok_or_else(|| anyhow!("songbird が初期化されていない"))?;

                Ok(Data {
                    speech: Arc::new(speech::Manager::new(engine, songbird)),
                    read_channels: RwLock::new(HashMap::new()),
                })
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

/// 起動時のヘルスチェックとウォームアップ（PLAN §7 補足）。
/// ENGINE が落ちていても Bot は起動させる（PLAN §10.3）。読み上げ時に失敗してログに残る。
async fn warm_up(engine: &voicevox::Client) {
    match engine.version().await {
        Ok(version) => tracing::info!(version = version.trim(), "voicevox engine is up"),
        Err(error) => {
            tracing::error!(%error, "voicevox engine is unreachable; 読み上げは失敗し続ける");
            return;
        }
    }

    let started = std::time::Instant::now();
    match engine.initialize_speaker(voicevox::DEFAULT_STYLE).await {
        Ok(()) => tracing::info!(
            style = %voicevox::DEFAULT_STYLE,
            elapsed_ms = started.elapsed().as_millis(),
            "speaker warmed up",
        ),
        Err(error) => tracing::warn!(%error, "warm-up failed; 初回発話が遅くなる"),
    }
}
