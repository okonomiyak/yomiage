mod commands;
mod db;
mod speech;
mod voicevox;

use std::sync::Arc;

use anyhow::{Context as _, anyhow};
use poise::serenity_prelude::{self as serenity};
use songbird::SerenityInit;
use tracing_subscriber::EnvFilter;

use crate::commands::settings::StyleChoice;
use crate::db::Db;
use crate::speech::SpeechTask;

/// アプリ層のエラーは anyhow に寄せる（PLAN §0）。
pub type Error = anyhow::Error;
pub type Context<'a> = poise::Context<'a, Data, Error>;

pub struct Data {
    pub db: Arc<Db>,
    pub speech: Arc<speech::Manager>,
    /// `/voice` のオートコンプリート用。起動時に `/speakers` から作る（PLAN §8）。
    /// ENGINE が落ちていれば空のままにして、コマンド側で検証を諦める。
    pub styles: Vec<StyleChoice>,
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
    ctx: &serenity::Context,
    event: &serenity::FullEvent,
    _framework: poise::FrameworkContext<'_, Data, Error>,
    data: &Data,
) -> Result<(), Error> {
    if let serenity::FullEvent::Message { new_message } = event {
        // 読み上げの失敗でイベント処理を落とさない。中でログに残して握る。
        handle_message(ctx, new_message, data).await;
    }
    Ok(())
}

/// メッセージを読み上げキューへ流す（PLAN §7 の 1〜2 と 6）。
/// テキスト正規化（§7-3）と辞書（§7-4）はフェーズ 4。
async fn handle_message(ctx: &serenity::Context, message: &serenity::Message, data: &Data) {
    let Some(guild_id) = message.guild_id else {
        return;
    };
    // 自分の発言は設定に関わらず読まない。
    if message.author.id == ctx.cache.current_user().id {
        return;
    }

    match data.db.is_read_channel(guild_id, message.channel_id).await {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            tracing::warn!(%guild_id, %error, "failed to check read channel");
            return;
        }
    }

    let settings = match data.db.guild_settings(guild_id).await {
        Ok(settings) => settings,
        Err(error) => {
            tracing::warn!(%guild_id, %error, "failed to load guild settings; using defaults");
            db::GuildSettings::default()
        }
    };

    if message.author.bot && !settings.read_bots {
        return;
    }

    let content = message.content.trim();
    if content.is_empty() || content.starts_with(&settings.ignore_prefix) {
        return;
    }
    let text: String = content.chars().take(settings.max_length).collect();

    let voice = match data.db.voice(message.author.id).await {
        Ok(voice) => voice,
        Err(error) => {
            tracing::warn!(user = %message.author.id, %error, "failed to load voice; using defaults");
            voicevox::Voice::default()
        }
    };

    data.speech
        .enqueue(guild_id, SpeechTask { text, voice })
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
    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:data/bot.db".to_owned());

    let engine = Arc::new(voicevox::Client::new(&voicevox_url).context("VOICEVOX_URL が不正")?);
    let database = Arc::new(Db::connect(&database_url).await?);
    tracing::info!(url = voicevox_url, database = database_url, "configured");

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

                let styles = warm_up(&engine).await;

                let songbird = songbird::get(ctx)
                    .await
                    .ok_or_else(|| anyhow!("songbird が初期化されていない"))?;

                Ok(Data {
                    db: database,
                    speech: Arc::new(speech::Manager::new(engine, songbird)),
                    styles,
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

/// 起動時のヘルスチェック、話者一覧の取得、ウォームアップ（PLAN §7 補足 / §8）。
/// ENGINE が落ちていても Bot は起動させる（PLAN §10.3）。読み上げ時に失敗してログに残る。
async fn warm_up(engine: &voicevox::Client) -> Vec<StyleChoice> {
    match engine.version().await {
        Ok(version) => tracing::info!(version, "voicevox engine is up"),
        Err(error) => {
            tracing::error!(%error, "voicevox engine is unreachable; 読み上げは失敗し続ける");
            return Vec::new();
        }
    }

    let styles = match engine.speakers().await {
        Ok(speakers) => speakers
            .into_iter()
            .flat_map(|speaker| {
                speaker.styles.into_iter().map(move |style| StyleChoice {
                    label: format!("{}（{}）", speaker.name, style.name),
                    id: style.id,
                })
            })
            .collect::<Vec<_>>(),
        Err(error) => {
            tracing::warn!(%error, "failed to fetch speakers; /voice の候補が出ない");
            Vec::new()
        }
    };
    tracing::info!(count = styles.len(), "speaker styles cached");

    let started = std::time::Instant::now();
    match engine.initialize_speaker(voicevox::DEFAULT_STYLE).await {
        Ok(()) => tracing::info!(
            style = %voicevox::DEFAULT_STYLE,
            elapsed_ms = started.elapsed().as_millis(),
            "speaker warmed up",
        ),
        Err(error) => tracing::warn!(%error, "warm-up failed; 初回発話が遅くなる"),
    }

    styles
}
