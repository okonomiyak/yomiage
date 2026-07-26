mod commands;
mod db;
mod speech;
mod text;
mod voicevox;

use std::sync::Arc;

use anyhow::Context as _;
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
    // どちらも失敗でイベント処理を落とさない。中でログに残して握る。
    match event {
        serenity::FullEvent::Message { new_message } => {
            handle_message(ctx, new_message, data).await;
        }
        serenity::FullEvent::VoiceStateUpdate { old, new } => {
            handle_voice_state(ctx, old.as_ref(), new, data).await;
        }
        _ => {}
    }
    Ok(())
}

/// 入退室アナウンスと自動退出（PLAN §7.1 / §2 v0.2）。
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
    let Some(call_lock) = data.speech.songbird().get(guild_id) else {
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
        // ミュートやカメラの切り替え。移動ではない。
        return;
    }

    let joined = after == Some(bot_channel);
    let left = before == Some(bot_channel);
    if !joined && !left {
        return;
    }

    if let Some(name) = display_name(new) {
        let action = if joined { "参加" } else { "退出" };
        let voice = data
            .db
            .voice(new.user_id)
            .await
            .unwrap_or_else(|_| voicevox::Voice::default());
        data.speech
            .enqueue(
                guild_id,
                speech::SpeechTask {
                    text: format!("{name}が{action}しました"),
                    voice,
                    origin: None,
                },
            )
            .await;
    }

    if left && should_leave(ctx, guild_id, bot_channel, me) {
        tracing::info!(%guild_id, "voice channel is empty; leaving");
        if let Err(error) = data.speech.songbird().remove(guild_id).await {
            tracing::warn!(%guild_id, %error, "failed to auto-leave");
        }
        data.speech.stop(guild_id).await;
        if let Err(error) = data.db.clear_read_channels(guild_id).await {
            tracing::warn!(%guild_id, %error, "failed to clear read channels on auto-leave");
        }
    }
}

/// 読み上げる名前は サーバーニックネーム > 表示名 > ユーザー名 の順（PLAN §7.1）。
/// 正規化を通して空になったらアナウンスしない。
fn display_name(state: &serenity::VoiceState) -> Option<String> {
    let raw = state
        .member
        .as_ref()
        .and_then(|member| {
            member
                .nick
                .clone()
                .or_else(|| member.user.global_name.clone())
                .or_else(|| Some(member.user.name.clone()))
        })
        .unwrap_or_default();

    text::normalize(&raw, &text::Options::default())
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
            // 他の Bot は「人」として数えない。
            && !state.member.as_ref().is_some_and(|member| member.user.bot)
    })
}

/// メッセージを読み上げキューへ流す（PLAN §7）。
/// フィルタ → 辞書 → 正規化 → キュー投入。
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
    if content.starts_with(&settings.ignore_prefix) {
        return;
    }

    let dictionary = data.db.dictionary(guild_id).await.unwrap_or_else(|error| {
        tracing::warn!(%guild_id, %error, "failed to load dictionary");
        Vec::new()
    });
    let names = resolve_names(ctx, message);

    // 正規化して読むものが無くなったら、そのメッセージは飛ばす（PLAN §7-3）。
    let Some(text) = text::normalize(
        content,
        &text::Options {
            max_length: settings.max_length,
            names: &names,
            dictionary: &dictionary,
            attachments: message.attachments.len(),
        },
    ) else {
        return;
    };

    let voice = match data.db.voice(message.author.id).await {
        Ok(voice) => voice,
        Err(error) => {
            tracing::warn!(user = %message.author.id, %error, "failed to load voice; using defaults");
            voicevox::Voice::default()
        }
    };

    data.speech
        .enqueue(
            guild_id,
            SpeechTask {
                text,
                voice,
                origin: Some(message.channel_id),
            },
        )
        .await;
}

/// メンションの解決に使う名前をキャッシュから集める（PLAN §7-3）。
/// キャッシュの参照は Send でないので、この関数の中で全部 String に落とす。
fn resolve_names(ctx: &serenity::Context, message: &serenity::Message) -> text::Names {
    let mut names = text::Names::default();

    for user in &message.mentions {
        names.users.insert(user.id.get(), user.name.clone());
    }

    let Some(guild_id) = message.guild_id else {
        return names;
    };
    let Some(guild) = ctx.cache.guild(guild_id) else {
        return names;
    };

    // サーバーニックネームがあればそちらを優先する。
    for user in &message.mentions {
        let name = guild
            .members
            .get(&user.id)
            .and_then(|member| member.nick.clone())
            .or_else(|| user.global_name.clone())
            .unwrap_or_else(|| user.name.clone());
        names.users.insert(user.id.get(), name);
    }
    for role_id in &message.mention_roles {
        if let Some(role) = guild.roles.get(role_id) {
            names.roles.insert(role_id.get(), role.name.clone());
        }
    }
    for (channel_id, channel) in &guild.channels {
        names
            .channels
            .insert(channel_id.get(), channel.name.clone());
    }

    names
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

    // 再起動直後はどの VC にも接続していない。前回の登録が残っていると
    // 「VC に居ないのに合成する」状態になるので消す（異常終了の後始末も兼ねる）。
    match database.clear_all_read_channels().await {
        Ok(0) => {}
        Ok(count) => tracing::info!(count, "cleared stale read channels from previous run"),
        Err(error) => tracing::warn!(%error, "failed to clear stale read channels"),
    }

    // Songbird を自分で作って登録する。こうしておくと終了処理から触れる。
    let songbird = songbird::Songbird::serenity();

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
        .setup({
            let database = database.clone();
            let songbird = songbird.clone();
            move |ctx, ready, framework| {
                Box::pin(async move {
                    register_commands(ctx, framework.options()).await?;
                    tracing::info!(user = %ready.user.name, "logged in");

                    let styles = warm_up(&engine).await;

                    Ok(Data {
                        db: database,
                        speech: Arc::new(speech::Manager::new(engine, songbird, ctx.http.clone())),
                        styles,
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
        shutdown_signal().await;
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

/// スラッシュコマンドを登録する。
///
/// `GUILD_ID` があればそのサーバーだけに登録する。ギルド登録は**即時反映**なので開発中はこちら。
/// 空ならグローバル登録で、こちらは反映に最大 1 時間かかる。
async fn register_commands(
    ctx: &serenity::Context,
    options: &poise::FrameworkOptions<Data, Error>,
) -> anyhow::Result<()> {
    let guild_id = std::env::var("GUILD_ID")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .map(serenity::GuildId::new);

    let Some(guild_id) = guild_id else {
        poise::builtins::register_globally(ctx, &options.commands).await?;
        tracing::info!(
            count = options.commands.len(),
            "commands registered globally"
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
async fn shutdown_signal() {
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
