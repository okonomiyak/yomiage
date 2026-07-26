//! ギルドごとの読み上げキュー（PLAN §6 / §7）。
//!
//! ギルドにつき 2 本のタスクを持つ。
//!
//! ```text
//!   enqueue ──▶ [text queue] ──▶ 合成タスク ──▶ [audio queue] ──▶ 再生タスク ──▶ songbird
//! ```
//!
//! audio queue にバッファを持たせることで、再生中に次の合成が走る（先読み合成 / PLAN §4.1）。
//! 合成の RTF は 0.3〜0.5 なので、一度流れ始めれば再生が合成を追い越すことはない。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use poise::serenity_prelude::{self as serenity, ChannelId, GuildId, async_trait};
use songbird::input::Input;
use songbird::tracks::TrackHandle;
use songbird::{Event, EventContext, EventHandler as VoiceEventHandler, Songbird};
use tokio::sync::{Mutex, Notify, Semaphore, mpsc};

use crate::voicevox::{self, Voice};

/// 未合成のテキストを溜める上限。連投で溢れたぶんは捨てる（PLAN §4「キュー長上限」）。
const TEXT_QUEUE_LIMIT: usize = 20;
/// 合成済み wav の先読み数。1 で「再生中に次を合成」になる。
const AUDIO_QUEUE_LIMIT: usize = 1;
/// ENGINE への同時リクエスト数。ENGINE の VV_CPU_NUM_THREADS=2 に合わせる。
const ENGINE_CONCURRENCY: usize = 2;
/// 再生完了イベントが来なかったときに諦めるまでの猶予。
const PLAYBACK_GRACE: Duration = Duration::from_secs(10);

pub struct SpeechTask {
    pub text: String,
    pub voice: Voice,
    /// 合成せずにこの wav をそのまま鳴らす（exVOICE）。
    pub file: Option<PathBuf>,
    /// 合成に失敗したときの通知先（PLAN §4）。入退室アナウンスなど、
    /// 通知先が無いものは None。
    pub origin: Option<ChannelId>,
}

pub struct Manager {
    engine: Arc<voicevox::Client>,
    songbird: Arc<Songbird>,
    /// ENGINE 障害を 1 度だけ通知するために使う（PLAN §4）。
    http: Arc<serenity::Http>,
    /// ENGINE を共有資源として絞る。ギルドが増えても合成が殺到しない。
    engine_limit: Arc<Semaphore>,
    queues: Mutex<HashMap<GuildId, mpsc::Sender<SpeechTask>>>,
    /// 今どのギルドで何を再生しているか。`/skip` から止めるために持つ。
    playing: Arc<Mutex<HashMap<GuildId, Playing>>>,
}

/// 再生中のトラックと、その完了を待っている側を起こすための通知。
struct Playing {
    track: TrackHandle,
    done: Arc<Notify>,
}

impl Manager {
    pub fn new(
        engine: Arc<voicevox::Client>,
        songbird: Arc<Songbird>,
        http: Arc<serenity::Http>,
    ) -> Self {
        Self {
            engine,
            songbird,
            http,
            engine_limit: Arc::new(Semaphore::new(ENGINE_CONCURRENCY)),
            queues: Mutex::new(HashMap::new()),
            playing: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 読み上げを積む。キューが詰まっていたら**待たずに捨てる**。
    /// ここで待つとイベントハンドラ全体が止まるため。
    pub async fn enqueue(&self, guild_id: GuildId, task: SpeechTask) {
        let mut queues = self.queues.lock().await;
        let sender = queues
            .entry(guild_id)
            .or_insert_with(|| self.spawn_pipeline(guild_id));

        match sender.try_send(task) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(%guild_id, "speech queue is full; dropping message");
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                // タスクが死んでいる。作り直して次の発言から復帰させる。
                tracing::warn!(%guild_id, "speech pipeline was closed; restarting");
                queues.remove(&guild_id);
            }
        }
    }

    /// 再生中の 1 件を打ち切って次へ進める（`/skip`）。止めるものが無ければ false。
    pub async fn skip(&self, guild_id: GuildId) -> bool {
        let Some(playing) = self.playing.lock().await.remove(&guild_id) else {
            return false;
        };
        // stop() だけだと完了イベントが来る保証が無く、待機側が
        // タイムアウトまで止まってしまう。こちらからも起こす。
        let _ = playing.track.stop();
        playing.done.notify_one();
        tracing::debug!(%guild_id, "playback skipped");
        true
    }

    pub fn songbird(&self) -> &Arc<Songbird> {
        &self.songbird
    }

    /// キューを破棄してタスクを止める（`/leave`）。
    pub async fn stop(&self, guild_id: GuildId) {
        if self.queues.lock().await.remove(&guild_id).is_some() {
            tracing::debug!(%guild_id, "speech pipeline dropped");
        }
    }

    fn spawn_pipeline(&self, guild_id: GuildId) -> mpsc::Sender<SpeechTask> {
        let (text_tx, mut text_rx) = mpsc::channel::<SpeechTask>(TEXT_QUEUE_LIMIT);
        let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<u8>>(AUDIO_QUEUE_LIMIT);

        // 合成タスク。1 件失敗しても止めず、そのメッセージだけ飛ばす。
        let engine = self.engine.clone();
        let engine_limit = self.engine_limit.clone();
        let http = self.http.clone();
        tokio::spawn(async move {
            // ENGINE が落ちている間、毎回通知すると荒れるので 1 度だけ出す（PLAN §4）。
            let mut reported = false;
            while let Some(task) = text_rx.recv().await {
                // 収録済み素材があるならそれを鳴らす。ENGINE は使わない。
                let wav = if let Some(path) = &task.file {
                    match tokio::fs::read(path).await {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            tracing::warn!(%guild_id, %error, path = %path.display(),
                                "failed to read exvoice file; skipping");
                            continue;
                        }
                    }
                } else {
                    let Ok(_permit) = engine_limit.acquire().await else {
                        break;
                    };
                    match engine.synthesize(&task.text, task.voice).await {
                        Ok(wav) => {
                            reported = false;
                            wav
                        }
                        Err(error) => {
                            tracing::warn!(%guild_id, %error, "synthesis failed; skipping message");
                            if !reported {
                                reported = true;
                                if let Some(channel) = task.origin {
                                    let notice = "音声合成に失敗しました。VOICEVOX ENGINE が                                                  応答していない可能性があります。                                                  復旧するまでこの通知は出しません。";
                                    if let Err(error) = channel.say(&http, notice).await {
                                        tracing::warn!(%guild_id, %error, "failed to report engine outage");
                                    }
                                }
                            }
                            continue;
                        }
                    }
                };
                if audio_tx.send(wav).await.is_err() {
                    break;
                }
            }
            tracing::debug!(%guild_id, "synthesis task finished");
        });

        // 再生タスク。1 件ずつ、再生完了を待ってから次へ。
        let songbird = self.songbird.clone();
        let playing = self.playing.clone();
        tokio::spawn(async move {
            while let Some(wav) = audio_rx.recv().await {
                if let Err(error) = play(&songbird, &playing, guild_id, wav).await {
                    tracing::warn!(%guild_id, %error, "playback failed; skipping");
                }
            }
            tracing::debug!(%guild_id, "playback task finished");
        });

        text_tx
    }
}

async fn play(
    songbird: &Songbird,
    playing: &Mutex<HashMap<GuildId, Playing>>,
    guild_id: GuildId,
    wav: Vec<u8>,
) -> anyhow::Result<()> {
    let Some(call_lock) = songbird.get(guild_id) else {
        anyhow::bail!("VC に接続していない");
    };

    let expected = expected_duration(&wav);
    let done = Arc::new(Notify::new());

    let track = {
        let mut call = call_lock.lock().await;
        let track = call.play_input(Input::from(wav));
        // 正常終了とエラーの両方で起こす。片方だけだと詰まる。
        track.add_event(Event::Track(songbird::TrackEvent::End), Done(done.clone()))?;
        track.add_event(
            Event::Track(songbird::TrackEvent::Error),
            Done(done.clone()),
        )?;
        track
    };

    playing.lock().await.insert(
        guild_id,
        Playing {
            track: track.clone(),
            done: done.clone(),
        },
    );

    // イベントが来ない事故（壊れた wav、ドライバの取りこぼし）でキューが止まらないよう保険をかける。
    if tokio::time::timeout(expected + PLAYBACK_GRACE, done.notified())
        .await
        .is_err()
    {
        tracing::warn!(%guild_id, ?expected, "track did not report completion; stopping it");
        let _ = track.stop();
    }
    playing.lock().await.remove(&guild_id);
    Ok(())
}

/// 再生時間の見積もり。完了イベントを取りこぼしたときの保険にしか使わない。
/// exVOICE の wav は 48kHz とは限らないので、ヘッダの byteRate から求める。
fn expected_duration(wav: &[u8]) -> Duration {
    let rate = if wav.len() >= 32 && &wav[0..4] == b"RIFF" {
        u32::from_le_bytes([wav[28], wav[29], wav[30], wav[31]])
    } else {
        0
    };
    let rate = if rate == 0 {
        voicevox::BYTES_PER_SEC
    } else {
        rate
    };
    Duration::from_secs_f64(wav.len() as f64 / f64::from(rate))
}

/// 再生完了を待っている側を起こすだけのイベントハンドラ。
struct Done(Arc<Notify>);

#[async_trait]
impl VoiceEventHandler for Done {
    async fn act(&self, _ctx: &EventContext<'_>) -> Option<Event> {
        self.0.notify_one();
        // 一度起こしたら用済み。
        Some(Event::Cancel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use songbird::input::codecs::{get_codec_registry, get_probe};

    /// ENGINE が返す wav を songbird が実際にデコードできるか。
    /// symphonia の wav / pcm 機能を Cargo.toml で有効にし忘れると、ここで落ちる。
    #[tokio::test]
    #[ignore = "ENGINE の起動が必要"]
    async fn engine_wav_is_playable_by_songbird() {
        let base =
            std::env::var("VOICEVOX_URL").unwrap_or_else(|_| "http://localhost:50021".to_owned());
        let engine = voicevox::Client::new(&base).expect("URL が不正");
        let wav = engine
            .synthesize("再生できるかのテストなのだ。", Voice::default())
            .await
            .expect("合成に失敗");

        let input = Input::from(wav);
        let playable = input
            .make_playable_async(get_codec_registry(), get_probe())
            .await;
        assert!(playable.is_ok(), "songbird が wav を再生可能にできない");
    }
}
