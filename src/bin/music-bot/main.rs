mod commands;

use std::sync::Arc;

use anyhow::Context as _;
use poise::serenity_prelude::{self as serenity};
use songbird::SerenityInit;
use tracing_subscriber::EnvFilter;
use yomiage_bot::db::Db;
use yomiage_bot::music;

pub type Error = yomiage_bot::Error;
pub type Context<'a> = poise::Context<'a, Data, Error>;

pub struct Data {
    pub db: Arc<Db>,
    pub music: Arc<music::Manager>,
    /// 音楽パネルのシークバーを進めるタスク（ギルドごとに 1 本）。
    pub panels: Arc<commands::dashboard::Panels>,
}

async fn event_handler(
    ctx: &serenity::Context,
    event: &serenity::FullEvent,
    _framework: poise::FrameworkContext<'_, Data, Error>,
    data: &Data,
) -> Result<(), Error> {
    // どちらも失敗でイベント処理を落とさない。中でログに残して握る。
    match event {
        serenity::FullEvent::VoiceStateUpdate { old, new } => {
            handle_voice_state(ctx, old.as_ref(), new, data).await;
        }
        serenity::FullEvent::InteractionCreate { interaction } => {
            // 音楽パネルのボタン。それ以外の interaction は poise が扱う。
            if let Some(component) = interaction.as_message_component() {
                commands::dashboard::handle_component(ctx, component, data).await;
            }
        }
        _ => {}
    }
    Ok(())
}

/// VC から人がいなくなったら自動退出する。読み上げ Bot と違ってアナウンスはしない
/// （音楽 Bot は喋らない）。
async fn handle_voice_state(
    ctx: &serenity::Context,
    old: Option<&serenity::VoiceState>,
    new: &serenity::VoiceState,
    data: &Data,
) {
    let Some(guild_id) = new.guild_id else {
        return;
    };
    let me = ctx.cache.current_user().id;
    if new.user_id == me {
        return;
    }

    // Bot が今どの VC に居るか。居なければ何もしない。
    let Some(call_lock) = data.music.songbird().get(guild_id) else {
        return;
    };
    let bot_channel = {
        let call = call_lock.lock().await;
        call.current_channel()
    };
    let Some(bot_channel) = bot_channel.map(|id| serenity::ChannelId::new(id.0.get())) else {
        return;
    };

    let before = old.and_then(|state| state.channel_id);
    let after = new.channel_id;
    if before == after {
        // ミュートやカメラの切り替え。移動ではないのでここで終わり。
        return;
    }

    let left = before == Some(bot_channel);
    if !left || !should_leave(ctx, guild_id, bot_channel, me) {
        return;
    }

    tracing::info!(%guild_id, "voice channel is empty; leaving");
    if let Err(error) = data.music.songbird().remove(guild_id).await {
        tracing::warn!(%guild_id, %error, "failed to auto-leave");
    }
    data.music.stop(guild_id).await;
}

/// Bot が居る VC に人が残っているか。残っていなければ自動退出する。
fn should_leave(
    ctx: &serenity::Context,
    guild_id: serenity::GuildId,
    bot_channel: serenity::ChannelId,
    me: serenity::UserId,
) -> bool {
    // キャッシュの参照は Send でないので、この関数の中で完結させる。
    let Some(guild) = ctx.cache.guild(guild_id) else {
        return false;
    };
    !guild.voice_states.values().any(|state| {
        state.channel_id == Some(bot_channel)
            && state.user_id != me
            // 他の Bot は「人」として数えない（読み上げ Bot も含む）。
            && !state.member.as_ref().is_some_and(|member| member.user.bot)
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // .env が無くても環境変数から読めればよい（Docker では compose が渡す）。
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,yomiage_bot=debug,music_bot=debug")),
        )
        .init();

    // 起動時の設定読み込みだけは失敗即終了でよい（PLAN §0）。
    let token = std::env::var("DISCORD_TOKEN").context("DISCORD_TOKEN が設定されていない")?;
    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:data/bot.db".to_owned());

    let database = Arc::new(Db::connect(&database_url).await?);
    tracing::info!(database = database_url, "configured");

    // Songbird を自分で作って登録する。こうしておくと終了処理から触れる。
    let songbird = songbird::Songbird::serenity();

    // 音楽 Bot はメッセージ本文を読まないので、読み上げ Bot と違って
    // MESSAGE_CONTENT / GUILD_MESSAGES は要らない（最小権限）。
    let intents = serenity::GatewayIntents::GUILDS | serenity::GatewayIntents::GUILD_VOICE_STATES;

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: commands::all(),
            on_error: |error| Box::pin(yomiage_bot::on_error(error)),
            event_handler: |ctx, event, framework, data| {
                Box::pin(event_handler(ctx, event, framework, data))
            },
            ..Default::default()
        })
        .setup({
            let database = database.clone();
            let songbird = songbird.clone();
            move |ctx, ready, framework| {
                Box::pin(async move {
                    yomiage_bot::register_commands(ctx, framework.options(), &ready.guilds).await?;
                    tracing::info!(user = %ready.user.name, "logged in");

                    let music = Arc::new(music::Manager::new(songbird, reqwest::Client::new()));

                    Ok(Data {
                        db: database,
                        music,
                        panels: Arc::default(),
                    })
                })
            }
        })
        .build();

    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .register_songbird_with(songbird.clone())
        .await
        .context("Discord クライアントの生成に失敗")?;

    // SIGTERM（docker restart / stop）と Ctrl-C で、VC を抜けてから終了する。
    // 抜けずに落ちると Discord 側にしばらく居座って見える。
    let shard_manager = client.shard_manager.clone();
    tokio::spawn(async move {
        yomiage_bot::shutdown_signal().await;
        tracing::info!("shutdown signal received; leaving voice channels");

        let guilds: Vec<_> = songbird.iter().map(|(guild_id, _)| guild_id).collect();
        for guild_id in guilds {
            if let Err(error) = songbird.remove(guild_id).await {
                tracing::warn!(?guild_id, %error, "failed to leave voice channel on shutdown");
            }
        }

        shard_manager.shutdown_all().await;
    });

    client.start().await.context("Discord クライアントが停止")?;

    Ok(())
}
