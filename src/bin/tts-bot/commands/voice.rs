use anyhow::anyhow;
use poise::serenity_prelude as serenity;

use crate::{Context, Error};

/// ボイスチャンネルに接続し、読み上げ対象を登録する。
///
/// 接続先は「このチャンネルに紐づいた VC」→「実行者が居る VC」の順に決める。
/// 読み上げ対象は、その VC に紐づいたテキスト ch があればそれ、無ければ実行チャンネル。
#[poise::command(slash_command, guild_only)]
pub async fn join(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only コマンドなのに guild_id が取れない"))?;
    let here = ctx.channel_id();
    let db = &ctx.data().db;

    let voice_channel = match db.bound_voice_channel(guild_id, here).await? {
        Some(bound) => Some(bound),
        None => {
            // Guild の参照（キャッシュ）は Send でないため、必要な値だけ取り出してスコープを抜ける。
            let Some(guild) = ctx.guild() else {
                ctx.say("サーバー情報を取得できませんでした。").await?;
                return Ok(());
            };
            guild
                .voice_states
                .get(&ctx.author().id)
                .and_then(|state| state.channel_id)
        }
    };

    let Some(voice_channel) = voice_channel else {
        ctx.say(
            "先にボイスチャンネルに参加するか、`/bind` でこのチャンネルに\
             ボイスチャンネルを紐づけてください。",
        )
        .await?;
        return Ok(());
    };

    // 紐づけ経由だと実行者が VC に居なくても接続先が決まる。無人の VC には入らない
    // （自動退出は誰かが抜けたときにしか判定しないので、入ると居座ってしまう）。
    if crate::humans_in(ctx.serenity_context(), guild_id, voice_channel) == Some(0) {
        ctx.say(format!("<#{voice_channel}> に誰もいないため参加しません。"))
            .await?;
        return Ok(());
    }

    let manager = songbird::get(ctx.serenity_context())
        .await
        .ok_or_else(|| anyhow!("songbird が初期化されていない"))?;

    // VC 接続のハンドシェイクで 3 秒を超えることがある。先に ack しておく。
    ctx.defer().await?;

    match manager.join(guild_id, voice_channel).await {
        Ok(_call) => {
            ctx.data().join_times.set(guild_id).await;

            // 紐づけがあるならそちらを優先する。無いときだけ実行チャンネルを登録する。
            let bound = db.bound_text_channels(guild_id, voice_channel).await?;
            let targets = if bound.is_empty() { vec![here] } else { bound };
            for channel in &targets {
                db.add_read_channel(guild_id, *channel).await?;
            }

            let list = targets
                .iter()
                .map(|id| format!("<#{id}>"))
                .collect::<Vec<_>>()
                .join(" ");
            tracing::info!(%guild_id, %voice_channel, targets = targets.len(), "joined voice channel");
            ctx.say(format!(
                "<#{voice_channel}> に参加しました。{list} の発言を読み上げます。"
            ))
            .await?;
        }
        Err(error) => {
            tracing::warn!(%guild_id, %voice_channel, ?error, "failed to join voice channel");
            ctx.say("ボイスチャンネルへの接続に失敗しました。").await?;
        }
    }

    Ok(())
}

/// ボイスチャンネルから切断する。
#[poise::command(slash_command, guild_only)]
pub async fn leave(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only コマンドなのに guild_id が取れない"))?;

    let manager = songbird::get(ctx.serenity_context())
        .await
        .ok_or_else(|| anyhow!("songbird が初期化されていない"))?;

    if manager.get(guild_id).is_none() {
        ctx.say("ボイスチャンネルに参加していません。").await?;
        return Ok(());
    }

    match manager.remove(guild_id).await {
        Ok(()) => {
            // キューを捨てて読み上げ対象を外す。紐づけ（/bind）は設定なので残す。
            ctx.data().speech.stop(guild_id).await;
            ctx.data().db.clear_read_channels(guild_id).await?;
            let _ = ctx.data().join_times.take(guild_id).await;

            tracing::info!(%guild_id, "left voice channel");
            ctx.say("ボイスチャンネルから切断しました。").await?;
        }
        Err(error) => {
            tracing::warn!(%guild_id, ?error, "failed to leave voice channel");
            ctx.say("切断に失敗しました。").await?;
        }
    }

    Ok(())
}

/// 接続中のボイスチャンネルを移動する。
///
/// `/leave` + `/join` と違い、キューや読み上げは止めずに繋ぎ直す。
/// 読み上げ対象は移動先の紐づけ（無ければ実行チャンネル）に差し替える。
#[poise::command(slash_command, guild_only, rename = "move")]
pub async fn move_channel(
    ctx: Context<'_>,
    #[description = "移動先のボイスチャンネル"]
    #[channel_types("Voice")]
    voice: serenity::GuildChannel,
) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only コマンドなのに guild_id が取れない"))?;

    let manager = songbird::get(ctx.serenity_context())
        .await
        .ok_or_else(|| anyhow!("songbird が初期化されていない"))?;

    let Some(call_lock) = manager.get(guild_id) else {
        ctx.say("ボイスチャンネルに参加していません。`/join` を使ってください。")
            .await?;
        return Ok(());
    };

    let current = {
        let call = call_lock.lock().await;
        call.current_channel()
    };
    let current = current.map(|id| serenity::ChannelId::new(id.0.get()));
    if current == Some(voice.id) {
        ctx.say("既にそのボイスチャンネルにいます。").await?;
        return Ok(());
    }

    // VC 接続のハンドシェイクで 3 秒を超えることがある。先に ack しておく。
    ctx.defer().await?;

    // 移動で発生する自分の VoiceStateUpdate を、外部からの強制移動と区別するための目印。
    ctx.data().expected_moves.mark(guild_id).await;

    match manager.join(guild_id, voice.id).await {
        Ok(_call) => {
            let db = &ctx.data().db;
            // 移動元の紐づけ先はもう読まないので、対象を移動先のものに差し替える。
            db.clear_read_channels(guild_id).await?;
            let bound = db.bound_text_channels(guild_id, voice.id).await?;
            let targets = if bound.is_empty() {
                vec![ctx.channel_id()]
            } else {
                bound
            };
            for channel in &targets {
                db.add_read_channel(guild_id, *channel).await?;
            }

            let list = targets
                .iter()
                .map(|id| format!("<#{id}>"))
                .collect::<Vec<_>>()
                .join(" ");
            tracing::info!(%guild_id, voice = %voice.id, "moved voice channel");
            ctx.say(format!(
                "<#{}> に移動しました。{list} の発言を読み上げます。",
                voice.id
            ))
            .await?;
        }
        Err(error) => {
            ctx.data().expected_moves.take(guild_id).await;
            tracing::warn!(%guild_id, voice = %voice.id, ?error, "failed to move voice channel");
            ctx.say("ボイスチャンネルへの移動に失敗しました。").await?;
        }
    }

    Ok(())
}

/// 今読み上げている 1 件を飛ばす。
#[poise::command(slash_command, guild_only)]
pub async fn skip(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only コマンドなのに guild_id が取れない"))?;

    if ctx.data().speech.skip(guild_id).await {
        ctx.say("スキップしました。").await?;
    } else {
        ctx.say("いま読み上げているものはありません。").await?;
    }
    Ok(())
}

/// テキストチャンネルをボイスチャンネルに紐づける。
///
/// 紐づけておくと、そのテキストチャンネルで `/join` するだけで対象の VC に繋がり、
/// そのチャンネルの発言が読み上げられる。設定なので再起動しても残る。
#[poise::command(slash_command, guild_only)]
pub async fn bind(
    ctx: Context<'_>,
    #[description = "読み上げたいテキストチャンネル"]
    #[channel_types("Text")]
    text: serenity::GuildChannel,
    #[description = "読み上げ先のボイスチャンネル"]
    #[channel_types("Voice")]
    voice: serenity::GuildChannel,
) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only コマンドなのに guild_id が取れない"))?;

    let bot_id = ctx.serenity_context().cache.current_user().id;
    let member = guild_id.member(ctx, bot_id).await?;
    let guild = guild_id.to_partial_guild(ctx).await?;
    let text_perms = guild.user_permissions_in(&text, &member);
    let voice_perms = guild.user_permissions_in(&voice, &member);

    if !text_perms.view_channel() {
        ctx.say(format!(
            "Bot に <#{}> を見る権限が無いため、紐づけを保存しませんでした。",
            text.id
        ))
        .await?;
        return Ok(());
    }
    if !voice_perms.view_channel() || !voice_perms.connect() {
        ctx.say(format!(
            "Bot に <#{}> へ接続する権限が無いため、紐づけを保存しませんでした。",
            voice.id
        ))
        .await?;
        return Ok(());
    }

    ctx.data()
        .db
        .bind_channel(guild_id, text.id, voice.id)
        .await?;

    tracing::info!(%guild_id, text = %text.id, voice = %voice.id, "channel bound");
    ctx.say(format!(
        "<#{}> を <#{}> に紐づけました。`/join` で接続すると読み上げます。",
        text.id, voice.id
    ))
    .await?;
    Ok(())
}

/// 紐づけを解除する。読み上げ中なら対象からも外す。
#[poise::command(slash_command, guild_only)]
pub async fn unbind(
    ctx: Context<'_>,
    #[description = "紐づけを外すテキストチャンネル"]
    #[channel_types("Text")]
    text: serenity::GuildChannel,
) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only コマンドなのに guild_id が取れない"))?;
    let db = &ctx.data().db;

    if db.unbind_channel(guild_id, text.id).await? {
        // 今読み上げ中なら、その対象からも外す。
        db.remove_read_channel(guild_id, text.id).await?;
        tracing::info!(%guild_id, text = %text.id, "channel unbound");
        ctx.say(format!("<#{}> の紐づけを解除しました。", text.id))
            .await?;
    } else {
        ctx.say(format!("<#{}> は紐づけられていません。", text.id))
            .await?;
    }
    Ok(())
}
