//! bind 済みの VC に人が入ったら「読み上げを入れませんか？」と提案する。
//!
//! ボタン押下は毎回コレクタで待たず、`InteractionCreate` を `custom_id` で
//! 振り分ける（music-bot の dashboard と同じ方式）。こうしておくと Bot を
//! 再起動しても、前に貼った案内メッセージのボタンがそのまま使える。

use poise::serenity_prelude as serenity;

use crate::Data;

const PREFIX: &str = "tts_join_prompt:";

/// bind 済みの VC に **1 人目**が入ったら、読み上げ参加を提案する。
/// 既に誰か居るところに追加で入った場合や、bind されていない VC、
/// 既にその VC で読み上げ中のときは何もしない。
pub async fn maybe_prompt(
    ctx: &serenity::Context,
    guild_id: serenity::GuildId,
    voice_channel: serenity::ChannelId,
    data: &Data,
) {
    if !is_first_human_here(ctx, guild_id, voice_channel) {
        return;
    }

    let enabled = data
        .db
        .guild_settings(guild_id)
        .await
        .is_ok_and(|settings| settings.tts_enabled);
    if !enabled {
        return;
    }

    let targets = match data.db.bound_text_channels(guild_id, voice_channel).await {
        Ok(targets) => targets,
        Err(error) => {
            tracing::warn!(%guild_id, %error, "failed to look up bound text channels");
            return;
        }
    };
    if targets.is_empty()
        || is_already_reading(data, guild_id, voice_channel).await
        || sub_bot_is_here(ctx, guild_id, voice_channel, data.sub_bot_id)
    {
        return;
    }

    let button = serenity::CreateButton::new(format!("{PREFIX}join:{voice_channel}"))
        .emoji('🎤')
        .label("参加する")
        .style(serenity::ButtonStyle::Primary);
    let content = format!("<#{voice_channel}> に参加者がいます。読み上げを入れませんか？");

    for text_channel in targets {
        let message = serenity::CreateMessage::new()
            .content(content.clone())
            .components(vec![serenity::CreateActionRow::Buttons(vec![
                button.clone(),
            ])]);
        if let Err(error) = text_channel.send_message(&ctx.http, message).await {
            tracing::warn!(%guild_id, %error, "failed to send join prompt");
        }
    }
}

/// その VC に居る人間（Bot 以外）がちょうど 1 人＝今入った本人だけかどうか。
/// 既に誰か居るところへの追加参加では、もう案内済みのはずなので出さない。
fn is_first_human_here(
    ctx: &serenity::Context,
    guild_id: serenity::GuildId,
    voice_channel: serenity::ChannelId,
) -> bool {
    crate::humans_in(ctx, guild_id, voice_channel) == Some(1)
}

/// sub bot が既にその VC に居るなら、メインからは案内しない（二重に読み上げが入るのを防ぐ）。
fn sub_bot_is_here(
    ctx: &serenity::Context,
    guild_id: serenity::GuildId,
    voice_channel: serenity::ChannelId,
    sub_bot_id: Option<serenity::UserId>,
) -> bool {
    let Some(sub_id) = sub_bot_id else {
        return false;
    };
    // キャッシュの参照は Send でないので、この関数の中で完結させる。
    let Some(guild) = ctx.cache.guild(guild_id) else {
        return false;
    };
    guild
        .voice_states
        .get(&sub_id)
        .and_then(|state| state.channel_id)
        == Some(voice_channel)
}

async fn is_already_reading(
    data: &Data,
    guild_id: serenity::GuildId,
    voice_channel: serenity::ChannelId,
) -> bool {
    let Some(call_lock) = data.speech.songbird().get(guild_id) else {
        return false;
    };
    let current = call_lock.lock().await.current_channel();
    current.map(|id| serenity::ChannelId::new(id.0.get())) == Some(voice_channel)
}

/// 案内メッセージのボタンが押されたときの処理。関係無いボタンは無視する。
pub async fn handle_component(
    ctx: &serenity::Context,
    interaction: &serenity::ComponentInteraction,
    data: &Data,
) {
    let Some(action) = interaction.data.custom_id.strip_prefix(PREFIX) else {
        return;
    };
    let Some(guild_id) = interaction.guild_id else {
        return;
    };

    if let Some(voice_channel) = action
        .strip_prefix("join:")
        .and_then(|id| id.parse::<u64>().ok())
        .map(serenity::ChannelId::new)
    {
        handle_join(ctx, interaction, guild_id, voice_channel, data).await;
    } else if action == "sub" {
        handle_sub_guide(ctx, interaction, guild_id, data).await;
    }
}

async fn handle_join(
    ctx: &serenity::Context,
    interaction: &serenity::ComponentInteraction,
    guild_id: serenity::GuildId,
    voice_channel: serenity::ChannelId,
    data: &Data,
) {
    let manager = data.speech.songbird();

    if let Some(call_lock) = manager.get(guild_id) {
        let current = call_lock.lock().await.current_channel();
        let current = current.map(|id| serenity::ChannelId::new(id.0.get()));
        if current == Some(voice_channel) {
            reply_ephemeral(ctx, interaction, "既に参加しています。").await;
            return;
        }

        // 他の VC で使用中。sub bot がこのサーバーに居ればそちらへ誘導するボタンを添える。
        let components = if data.sub_bot_id.is_some() {
            vec![serenity::CreateActionRow::Buttons(vec![
                serenity::CreateButton::new(format!("{PREFIX}sub"))
                    .emoji('🔀')
                    .label("予備 Bot に参加してもらう")
                    .style(serenity::ButtonStyle::Secondary),
            ])]
        } else {
            vec![]
        };
        let response = serenity::CreateInteractionResponse::Message(
            serenity::CreateInteractionResponseMessage::new()
                .content("他のボイスチャンネルで使用中です。")
                .components(components)
                .ephemeral(true),
        );
        if let Err(error) = interaction.create_response(&ctx.http, response).await {
            tracing::warn!(%guild_id, %error, "failed to respond to join prompt");
        }
        return;
    }

    // 案内を出した後に全員抜けていたら入らない。無人の VC に居座ることになる。
    if crate::humans_in(ctx, guild_id, voice_channel) == Some(0) {
        reply_ephemeral(
            ctx,
            interaction,
            "ボイスチャンネルに誰もいないため参加しません。",
        )
        .await;
        return;
    }

    // VC 接続のハンドシェイクで 3 秒を超えることがある。先に ack しておく。
    if let Err(error) = interaction
        .create_response(&ctx.http, serenity::CreateInteractionResponse::Acknowledge)
        .await
    {
        tracing::warn!(%guild_id, %error, "failed to acknowledge join prompt");
        return;
    }

    match manager.join(guild_id, voice_channel).await {
        Ok(_call) => {
            data.join_times.set(guild_id).await;

            // 紐づけがあるならそちらを優先する。無いときだけ案内を投げたチャンネルを登録する。
            let bound = data
                .db
                .bound_text_channels(guild_id, voice_channel)
                .await
                .unwrap_or_default();
            let targets = if bound.is_empty() {
                vec![interaction.channel_id]
            } else {
                bound
            };
            for channel in &targets {
                if let Err(error) = data.db.add_read_channel(guild_id, *channel).await {
                    tracing::warn!(%guild_id, %error, "failed to register read channel");
                }
            }
            tracing::info!(%guild_id, %voice_channel, "joined voice channel via prompt");

            let list = targets
                .iter()
                .map(|id| format!("<#{id}>"))
                .collect::<Vec<_>>()
                .join(" ");
            let edit = serenity::EditMessage::new()
                .content(format!(
                    "<#{voice_channel}> に参加しました。{list} の発言を読み上げます。"
                ))
                .components(vec![]);
            if let Err(error) = interaction
                .channel_id
                .edit_message(&ctx.http, interaction.message.id, edit)
                .await
            {
                tracing::warn!(%guild_id, %error, "failed to update join prompt");
            }
        }
        Err(error) => {
            tracing::warn!(%guild_id, %voice_channel, %error, "failed to join voice channel from prompt");
        }
    }
}

/// 自動連携はしない（PLAN 未決）。sub bot 自身の `/join` を叩いてもらう案内だけ返す。
async fn handle_sub_guide(
    ctx: &serenity::Context,
    interaction: &serenity::ComponentInteraction,
    guild_id: serenity::GuildId,
    data: &Data,
) {
    let Some(sub_id) = data.sub_bot_id else {
        return;
    };
    let content = if guild_id.member(ctx, sub_id).await.is_ok() {
        format!("<@{sub_id}> でこのチャンネルから `/join` を実行してください。")
    } else {
        "予備 Bot がこのサーバーに参加していません。".to_owned()
    };
    reply_ephemeral(ctx, interaction, &content).await;
}

async fn reply_ephemeral(
    ctx: &serenity::Context,
    interaction: &serenity::ComponentInteraction,
    content: &str,
) {
    let response = serenity::CreateInteractionResponse::Message(
        serenity::CreateInteractionResponseMessage::new()
            .content(content)
            .ephemeral(true),
    );
    if let Err(error) = interaction.create_response(&ctx.http, response).await {
        tracing::warn!(%error, "failed to reply to join prompt interaction");
    }
}
