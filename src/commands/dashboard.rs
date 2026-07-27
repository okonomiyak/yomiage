//! 音楽の操作パネル。
//!
//! ボタンの押下は毎回コレクタで待つのではなく、`InteractionCreate` を
//! `custom_id` で振り分けて処理する。こうしておくと Bot を再起動しても、
//! 前に貼ったパネルがそのまま使える。
//!
//! シークバーを動かすため、パネルを貼ったギルドには **5 秒ごとに本文を
//! 描き替えるタスク**（[`Panels`]）を 1 本だけ走らせる。内容が変わらないときは
//! 編集リクエストを出さないので、一時停止中や無音のときは静かになる。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use poise::serenity_prelude as serenity;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::db::Db;
use crate::music;
use crate::{Context, Data, Error};

/// このパネルのボタンだと分かるようにする接頭辞。
const PREFIX: &str = "music:";

/// 一覧に出す待機曲の数。パネルなので短めにする。
const QUEUE_PREVIEW: usize = 5;

/// ⏪ ⏩ で動かす幅（秒）。
/// 後方シークは 1 回ごとに yt-dlp からの取り直しになるので、連打すると
/// そのぶん待たされる（`music::seek_relative`）。
const SEEK_STEP_SECS: i64 = 10;

/// バーを描き替える間隔。15 目盛りなので、4 分の曲なら 1 目盛り 16 秒。
/// これより短くしても見た目は変わらず、編集リクエストが増えるだけ。
const TICK: Duration = Duration::from_secs(5);

/// 何も流れないまま空振りが続いたらタスクを畳む（5 秒 × 60 = 5 分）。
/// 放置されたパネルを永久に追い掛けないため。ボタンを押せば再開する。
const IDLE_TICKS: u32 = 60;

/// 描き替えタスクに渡す道具一式。
///
/// タスクは `Data` を借りられない（`&Data` の寿命がコマンドの実行中しかない）ので、
/// 必要な `Arc` だけ複製して持たせる。
#[derive(Clone)]
pub struct PanelCtx {
    music: Arc<music::Manager>,
    db: Arc<Db>,
    http: Arc<serenity::Http>,
}

impl PanelCtx {
    pub fn new(data: &Data, http: Arc<serenity::Http>) -> Self {
        Self {
            music: data.music.clone(),
            db: data.db.clone(),
            http,
        }
    }
}

/// ギルドごとに 1 本だけ走らせる、パネルの描き替えタスクの置き場。
#[derive(Default)]
pub struct Panels {
    tasks: Mutex<HashMap<serenity::GuildId, JoinHandle<()>>>,
}

impl Panels {
    /// このパネルを追い掛け始める。同じギルドの古いパネルは追うのをやめる。
    ///
    /// ボタンが押されるたびに呼ぶので、アイドルで畳まれた後でも復活する。
    pub async fn start(
        &self,
        panel: PanelCtx,
        guild_id: serenity::GuildId,
        channel: serenity::ChannelId,
        message: serenity::MessageId,
    ) {
        let mut tasks = self.tasks.lock().await;
        // 自分で終わったタスクの handle を溜めない。
        tasks.retain(|_, task| !task.is_finished());

        let task = tokio::spawn(follow(panel, guild_id, channel, message));
        if let Some(previous) = tasks.insert(guild_id, task) {
            // 1 ギルドに 2 本走らせない。古いパネルは押されたときに復活する。
            previous.abort();
        }
    }
}

/// パネルを 5 秒ごとに描き替え続ける。
async fn follow(
    panel: PanelCtx,
    guild_id: serenity::GuildId,
    channel: serenity::ChannelId,
    message: serenity::MessageId,
) {
    tracing::debug!(%guild_id, %message, "dashboard ticker started");

    let mut last: Option<String> = None;
    let mut idle = 0_u32;

    loop {
        tokio::time::sleep(TICK).await;

        let view = render(&panel, guild_id).await;
        if view.playing {
            idle = 0;
        } else {
            idle += 1;
            // 空振りが続いても、最後の 1 回は「再生していません」を描いてから畳む。
            if idle > IDLE_TICKS {
                tracing::debug!(%guild_id, "dashboard ticker stopped; idle");
                break;
            }
        }

        // 変わっていないなら編集しない。一時停止中と無音のときに無駄に叩かないため。
        if last.as_deref() == Some(view.content.as_str()) {
            continue;
        }

        let edit = serenity::EditMessage::new()
            .content(view.content.clone())
            .components(view.components);
        if let Err(error) = channel.edit_message(&panel.http, message, edit).await {
            // パネルが消された、権限が無い、など。追い掛け続けても直らない。
            tracing::debug!(%guild_id, %error, "dashboard ticker stopped; panel is not editable");
            break;
        }
        last = Some(view.content);
    }
}

/// 音楽の操作パネルを出す。
#[poise::command(slash_command, guild_only)]
pub async fn dashboard(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| anyhow!("guild_only コマンドなのに guild_id が取れない"))?;

    if !super::music::enabled(ctx, guild_id).await {
        ctx.say("音楽機能は無効です。`/feature` で有効にできます。")
            .await?;
        return Ok(());
    }

    let panel = PanelCtx::new(ctx.data(), ctx.serenity_context().http.clone());
    let view = render(&panel, guild_id).await;
    let handle = ctx
        .send(
            poise::CreateReply::default()
                .content(view.content)
                .components(view.components),
        )
        .await?;

    // バーを動かすには、貼ったメッセージを後から編集し続ける必要がある。
    // interaction のトークンは 15 分で切れるので、メッセージ ID を取って直接編集する。
    let message = handle.into_message().await?;
    ctx.data()
        .panels
        .start(panel, guild_id, message.channel_id, message.id)
        .await;

    Ok(())
}

/// パネルのボタンが押されたときの処理。押されていないものは無視する。
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

    let panel = PanelCtx::new(data, ctx.http.clone());

    // シークは後方だと yt-dlp から取り直しになり 3 秒を超える（music::seek_relative）。
    // 先に ack だけ返さないと Discord が「失敗しました」を出す。
    let seek = match action {
        "back" => Some(-SEEK_STEP_SECS),
        "forward" => Some(SEEK_STEP_SECS),
        _ => None,
    };

    if let Some(delta) = seek {
        let ack = serenity::CreateInteractionResponse::Acknowledge;
        if let Err(error) = interaction.create_response(&ctx.http, ack).await {
            tracing::warn!(%guild_id, %error, "failed to acknowledge seek");
            return;
        }

        data.music.seek_relative(guild_id, delta).await;
        tracing::info!(%guild_id, action, user = %interaction.user.id, "dashboard used");

        // ack 済みなので UpdateMessage は使えない。メッセージを直接編集する。
        let view = render(&panel, guild_id).await;
        let edit = serenity::EditMessage::new()
            .content(view.content)
            .components(view.components);
        if let Err(error) = interaction
            .channel_id
            .edit_message(&ctx.http, interaction.message.id, edit)
            .await
        {
            tracing::warn!(%guild_id, %error, "failed to update dashboard after seek");
        }
    } else {
        match action {
            "toggle" => {
                data.music.toggle_pause(guild_id).await;
            }
            "next" => {
                data.music.skip(guild_id).await;
            }
            "stop" => {
                data.music.stop(guild_id).await;
            }
            // 押しただけで最新の状態に描き替わる。
            "refresh" => {}
            other => {
                tracing::debug!(action = other, "unknown dashboard action");
                return;
            }
        }
        tracing::info!(%guild_id, action, user = %interaction.user.id, "dashboard used");

        let view = render(&panel, guild_id).await;
        let response = serenity::CreateInteractionResponse::UpdateMessage(
            serenity::CreateInteractionResponseMessage::new()
                .content(view.content)
                .components(view.components),
        );
        if let Err(error) = interaction.create_response(&ctx.http, response).await {
            tracing::warn!(%guild_id, %error, "failed to update dashboard");
        }
    }

    // 押されたパネルを追い掛け直す。アイドルで畳まれた後や Bot の再起動後でも、
    // ボタンを 1 回押せばバーがまた動き出す。
    data.panels
        .start(
            panel,
            guild_id,
            interaction.channel_id,
            interaction.message.id,
        )
        .await;
}

/// 描き替えの 1 コマ。
struct View {
    content: String,
    components: Vec<serenity::CreateActionRow>,
    /// 何か流れているか。タスクをいつ畳むかの判断だけに使う。
    playing: bool,
}

/// 今の状態から本文とボタンを作る。押されるたび・5 秒ごとに作り直す。
async fn render(panel: &PanelCtx, guild_id: serenity::GuildId) -> View {
    let now = panel.music.now_playing(guild_id).await;
    let titles = panel.music.queue(guild_id).await;
    let volume = panel
        .db
        .guild_settings(guild_id)
        .await
        .map_or(0.3, |settings| settings.music_volume);
    let percent = (volume * 100.0).round() as u32;
    let paused = now.as_ref().is_some_and(|now| now.paused);

    let content = match &now {
        None => "**音楽コントロール**\n再生していません。`/play` で追加してください。".to_owned(),
        Some(now) => {
            let state = if now.paused {
                "⏸ 一時停止中"
            } else {
                "▶ 再生中"
            };
            let mut body = format!(
                "**音楽コントロール**（音量 {percent}%）\n{state}: **{}**\n{}",
                now.title,
                music::progress_bar(now.position, now.duration),
            );
            // 先頭は再生中の曲なので、待機分だけ並べる。
            let waiting = titles.len().saturating_sub(1);
            for (index, title) in titles.iter().skip(1).take(QUEUE_PREVIEW).enumerate() {
                body.push_str(&format!("\n{}. {title}", index + 2));
            }
            if waiting > QUEUE_PREVIEW {
                body.push_str(&format!("\n…ほか {} 件", waiting - QUEUE_PREVIEW));
            }
            body
        }
    };

    // 1 行 5 個まで置けるが、絵文字付きだとスマホで詰まる。役割で 2 行に分ける。
    let seeking = serenity::CreateActionRow::Buttons(vec![
        serenity::CreateButton::new(format!("{PREFIX}back"))
            .emoji('⏪')
            .label(format!("{SEEK_STEP_SECS}秒"))
            .style(serenity::ButtonStyle::Secondary),
        serenity::CreateButton::new(format!("{PREFIX}toggle"))
            .emoji(if paused { '▶' } else { '⏸' })
            .label(if paused { "再開" } else { "一時停止" })
            .style(serenity::ButtonStyle::Primary),
        serenity::CreateButton::new(format!("{PREFIX}forward"))
            .emoji('⏩')
            .label(format!("{SEEK_STEP_SECS}秒"))
            .style(serenity::ButtonStyle::Secondary),
    ]);
    let queueing = serenity::CreateActionRow::Buttons(vec![
        serenity::CreateButton::new(format!("{PREFIX}next"))
            .emoji('⏭')
            .label("次へ")
            .style(serenity::ButtonStyle::Secondary),
        serenity::CreateButton::new(format!("{PREFIX}stop"))
            .emoji('⏹')
            .label("停止")
            .style(serenity::ButtonStyle::Danger),
        serenity::CreateButton::new(format!("{PREFIX}refresh"))
            .emoji('🔄')
            .label("更新")
            .style(serenity::ButtonStyle::Secondary),
    ]);

    View {
        content,
        components: vec![seeking, queueing],
        playing: now.is_some(),
    }
}
