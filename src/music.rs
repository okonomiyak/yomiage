//! 音楽再生（yt-dlp）。
//!
//! 読み上げとは別のトラックとして流す。songbird は 1 つの Call に複数トラックを
//! 混ぜられるので、音楽の上に読み上げが乗る形になる。音量を既定で小さめにしてあるのはそのため。
//!
//! キューは songbird の `builtin-queue` に任せる。自前で持つと再生完了の検知と
//! 状態の同期を自分で書くことになり、ずれの原因になる。読み上げは `play_input` で
//! 直接鳴らしているのでキューには入らず、音楽とは独立して動く。

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use async_trait::async_trait;
use poise::serenity_prelude::GuildId;
use songbird::input::{Compose, HttpRequest, Input, YoutubeDl};
use songbird::tracks::TrackHandle;
use songbird::{Call, Event, EventContext, EventHandler, Songbird, TrackEvent};
use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use crate::nicovideo::{self, CookieCopy};

/// タイトルも長さも取れなかった曲の表示。
const UNKNOWN_TITLE: &str = "（タイトル不明）";

/// シークで曲の終端に着地しないよう手前に残す余白。
/// 終端ちょうどへ飛ばすと、シークが終わった瞬間に曲が終わる。
const SEEK_TAIL_MARGIN: Duration = Duration::from_secs(3);

/// songbird が使うものと同じ実行ファイル名。
const YTDLP: &str = "yt-dlp";

/// 再生リストから 1 回の `/play` で積む曲数の上限（PLAN §13-12）。
/// キューが一気に埋まらないよう、超えた分は捨てて件数だけ伝える。
pub const PLAYLIST_LIMIT: usize = 50;

pub struct Manager {
    songbird: Arc<Songbird>,
    /// yt-dlp が返したストリーム URL を取りに行くのに使う。
    http: reqwest::Client,
    /// トラック UUID → 曲の情報。songbird 0.6 の `TrackHandle::data` は型が違うと
    /// panic するので使わない（読み上げのトラックと混ざる余地を残さない）。
    /// `TrackEndAnnouncer`（別タスクではなく songbird のイベントハンドラ）からも
    /// 参照するので `Arc` で持つ。
    tracks: Arc<Mutex<HashMap<Uuid, TrackInfo>>>,
    /// 曲がキューの中で自動的に切り替わったことを Discord 層に伝える送信側。
    changes: mpsc::UnboundedSender<TrackChanged>,
}

/// 曲がキューの中で自動的に切り替わったときの通知（PLAN §13-15）。
///
/// `/play` の直後は既にコマンドの返信で「▶ 再生します」と分かるので、ここで
/// 知らせるのは**キューの自動進行だけ**（前の曲が終わって次が始まった、または
/// 尽きた）。`/stop` で明示的に止めたときは出さない
/// （`Manager::stop` が `tracks` を先に空にするため、`TrackEndAnnouncer` 側で
/// 情報が見つからず何もしない）。
pub struct TrackChanged {
    pub guild_id: GuildId,
    pub finished: QueuedTrack,
    /// 続けて再生を始めた曲。無ければキューが尽きた。
    pub next: Option<QueuedTrack>,
}

/// キューに積んだ曲の付随情報。songbird 側からは取れないので自前で持つ。
struct TrackInfo {
    title: String,
    /// yt-dlp が返した曲の長さ。ライブ配信などでは取れない。
    duration: Option<Duration>,
    /// 元のページ（`webpage_url`）。検索で入れた曲でも実際に選ばれた動画の URL が入る。
    url: Option<String>,
}

impl TrackInfo {
    fn as_queued(&self) -> QueuedTrack {
        QueuedTrack {
            title: self.title.clone(),
            url: self.url.clone(),
        }
    }

    /// 表に無いトラック（`/play` を経ずに積まれた、情報を取りこぼした等）。
    fn unknown_track() -> QueuedTrack {
        QueuedTrack {
            title: UNKNOWN_TITLE.to_owned(),
            url: None,
        }
    }
}

/// 一覧に出す 1 曲。
#[derive(Clone)]
pub struct QueuedTrack {
    pub title: String,
    pub url: Option<String>,
}

impl QueuedTrack {
    /// 曲名を押すと元のページへ飛べる形にする。
    pub fn link(&self) -> String {
        track_link(&self.title, self.url.as_deref())
    }
}

/// キューに積んだ結果。
pub struct Queued {
    pub track: QueuedTrack,
    /// キューの何番目か。1 なら即再生。
    pub position: usize,
}

/// 再生リストをまとめてキューに積んだ結果。
pub struct PlaylistQueued {
    /// 実際に積めた曲数。
    pub queued: usize,
    /// 上限（`PLAYLIST_LIMIT`）を超えたために積まなかった曲数。
    pub skipped: usize,
    /// ニコニコの事前確認で再生できないと分かり、積まなかった曲数。
    /// YouTube は事前確認しないので常に 0。
    pub unplayable: usize,
}

/// 今流れている曲の状態。パネルの描画に要るものだけ。
pub struct NowPlaying {
    pub track: QueuedTrack,
    pub position: Duration,
    /// 取れないことがある（ライブ配信など）。その場合はバーを出さない。
    pub duration: Option<Duration>,
    pub paused: bool,
}

impl Manager {
    /// 戻り値の受信側は、自動進行の通知を読んで Discord に流し続けるタスクに渡すこと。
    pub fn new(
        songbird: Arc<Songbird>,
        http: reqwest::Client,
    ) -> (Self, mpsc::UnboundedReceiver<TrackChanged>) {
        let (changes, receiver) = mpsc::unbounded_channel();
        (
            Self {
                songbird,
                http,
                tracks: Arc::new(Mutex::new(HashMap::new())),
                changes,
            },
            receiver,
        )
    }

    pub fn songbird(&self) -> &Arc<Songbird> {
        &self.songbird
    }

    /// URL ならそのまま、そうでなければ検索語として扱う。空きがあれば即再生、なければ待機。
    pub async fn enqueue(
        &self,
        guild_id: GuildId,
        query: &str,
        volume: f32,
    ) -> anyhow::Result<Queued> {
        let call_lock = self
            .songbird
            .get(guild_id)
            .context("ボイスチャンネルに接続していない")?;

        let is_url = query.starts_with("http://") || query.starts_with("https://");

        // ニコニコだけは yt-dlp に直接ダウンロードさせる。songbird に URL を渡すと
        // Cookie が足りず 403 になるため（理由は nicovideo モジュールの説明を参照）。
        let (input, mut info) = if nicovideo::is_nicovideo(query) {
            let mut source = nicovideo::NicoVideo::new(query.to_owned());
            // メタデータも実際の取得も同じ yt-dlp なので、**ここで引けない動画は
            // 再生もできない**（会員限定・センシティブ指定など）。黙って積むと
            // 「再生します」と出したきり無音になり、理由が誰にも伝わらない。
            let metadata = source.aux_metadata().await.map_err(|error| {
                tracing::info!(%guild_id, %error, "nicovideo is not playable");
                anyhow::anyhow!("{}", nicovideo::readable_error(&error.to_string()))
            })?;
            let info = TrackInfo {
                title: metadata.title.unwrap_or_else(|| query.to_owned()),
                duration: metadata.duration,
                url: metadata.source_url,
            };
            (Input::Lazy(Box::new(source)), info)
        } else {
            let mut source = if is_url {
                YoutubeDl::new(self.http.clone(), query.to_owned())
            } else {
                YoutubeDl::new_search(self.http.clone(), query.to_owned())
            };
            let info = describe(&mut source, guild_id, query).await;
            (Input::from(source), info)
        };

        // メタデータを引けなくても、URL 指定なら少なくともリンクは張れる。
        if info.url.is_none() && is_url {
            info.url = Some(query.to_owned());
        }

        Ok(self
            .queue_input(guild_id, &call_lock, input, info, volume)
            .await)
    }

    /// アップロードされた MP3 をそのまま再生する（yt-dlp を介さない）。
    /// `url` は Discord の添付ファイル URL（`Attachment::url`）を想定している。
    pub async fn enqueue_upload(
        &self,
        guild_id: GuildId,
        url: &str,
        title: String,
        volume: f32,
    ) -> anyhow::Result<Queued> {
        let call_lock = self
            .songbird
            .get(guild_id)
            .context("ボイスチャンネルに接続していない")?;

        let input: Input = HttpRequest::new(self.http.clone(), url.to_owned()).into();
        let info = TrackInfo {
            title,
            duration: None,
            url: Some(url.to_owned()),
        };

        Ok(self
            .queue_input(guild_id, &call_lock, input, info, volume)
            .await)
    }

    /// 積んだ入力を Call のキューへ入れ、音量を反映して結果を返す共通処理
    /// （`enqueue` と `enqueue_upload` で共有）。
    async fn queue_input(
        &self,
        guild_id: GuildId,
        call_lock: &Arc<Mutex<Call>>,
        input: Input,
        info: TrackInfo,
        volume: f32,
    ) -> Queued {
        let (handle, position) = {
            let mut call = call_lock.lock().await;
            let handle = call.enqueue_input(input).await;
            (handle, call.queue().len())
        };

        if let Err(error) = handle.set_volume(volume) {
            tracing::warn!(%guild_id, %error, "failed to apply volume");
        }
        tracing::info!(
            %guild_id,
            position,
            title = info.title.as_str(),
            duration_secs = info.duration.map(|value| value.as_secs()),
            "music queued",
        );

        let track = info.as_queued();
        self.tracks.lock().await.insert(handle.uuid(), info);
        self.watch_for_end(guild_id, call_lock, &handle);
        Queued { track, position }
    }

    /// この曲が終わったときに次の曲（または空になったこと）を通知できるようにする。
    /// キューへ積んで `tracks` に情報を入れた直後に呼ぶこと。
    fn watch_for_end(&self, guild_id: GuildId, call_lock: &Arc<Mutex<Call>>, handle: &TrackHandle) {
        let announcer = TrackEndAnnouncer {
            guild_id,
            uuid: handle.uuid(),
            tracks: self.tracks.clone(),
            call: call_lock.clone(),
            changes: self.changes.clone(),
        };
        if let Err(error) = handle.add_event(Event::Track(TrackEvent::End), announcer) {
            tracing::warn!(%guild_id, %error, "failed to watch for track end");
        }
    }

    /// (再生中, 待機中) の一覧。
    pub async fn queue(&self, guild_id: GuildId) -> Vec<QueuedTrack> {
        let Some(call_lock) = self.songbird.get(guild_id) else {
            return Vec::new();
        };
        let handles: Vec<TrackHandle> = {
            let call = call_lock.lock().await;
            call.queue().current_queue()
        };

        let mut known = self.tracks.lock().await;
        // キューから消えた曲の情報は捨てる。放っておくと溜まり続ける。
        known.retain(|uuid, _| handles.iter().any(|handle| handle.uuid() == *uuid));

        handles
            .iter()
            .map(|handle| {
                known
                    .get(&handle.uuid())
                    .map_or_else(TrackInfo::unknown_track, TrackInfo::as_queued)
            })
            .collect()
    }

    /// 今流れている曲と再生位置。流れていなければ None。
    pub async fn now_playing(&self, guild_id: GuildId) -> Option<NowPlaying> {
        let call_lock = self.songbird.get(guild_id)?;
        let current = {
            let call = call_lock.lock().await;
            call.queue().current()
        }?;

        // 取れなければ「流れていない」扱いにする。パネルが古い位置で止まるより良い。
        let state = current.get_info().await.ok()?;

        let known = self.tracks.lock().await;
        let info = known.get(&current.uuid());
        Some(NowPlaying {
            track: info.map_or_else(TrackInfo::unknown_track, TrackInfo::as_queued),
            position: state.position,
            duration: info.and_then(|info| info.duration),
            paused: state.playing == songbird::tracks::PlayMode::Pause,
        })
    }

    /// 再生位置を前後に動かす。動かせたら着地した位置を返す。
    ///
    /// YouTube 音源は `HttpStream::is_seekable()` が false なので、**後方シークは
    /// songbird が `Compose`（yt-dlp）から取り直す**。数秒かかるので、呼び出し側は
    /// 先に interaction を ack しておくこと。
    pub async fn seek_relative(&self, guild_id: GuildId, delta_secs: i64) -> Option<Duration> {
        let call_lock = self.songbird.get(guild_id)?;
        let current = {
            let call = call_lock.lock().await;
            call.queue().current()
        }?;

        let position = current.get_info().await.ok()?.position;
        let step = Duration::from_secs(delta_secs.unsigned_abs());
        let mut target = if delta_secs >= 0 {
            position.saturating_add(step)
        } else {
            position.saturating_sub(step)
        };

        // 終端へ飛ばすとシーク直後に曲が終わってしまうので、少し手前で止める。
        let total = self
            .tracks
            .lock()
            .await
            .get(&current.uuid())
            .and_then(|info| info.duration);
        if let Some(total) = total
            && target.saturating_add(SEEK_TAIL_MARGIN) > total
        {
            target = total.saturating_sub(SEEK_TAIL_MARGIN);
        }

        match current.seek_async(target).await {
            Ok(reached) => {
                tracing::info!(
                    %guild_id,
                    delta_secs,
                    reached_secs = reached.as_secs(),
                    "music seeked",
                );
                Some(reached)
            }
            Err(error) => {
                tracing::warn!(%guild_id, %error, delta_secs, "failed to seek");
                None
            }
        }
    }

    /// 一時停止と再開を切り替える。切り替え後に一時停止中なら true を返す。
    /// 流れているものが無ければ None。
    pub async fn toggle_pause(&self, guild_id: GuildId) -> Option<bool> {
        let call_lock = self.songbird.get(guild_id)?;
        let queue = {
            let call = call_lock.lock().await;
            if call.queue().is_empty() {
                return None;
            }
            call.queue().clone()
        };

        let paused = is_paused(&queue).await;
        let result = if paused {
            queue.resume()
        } else {
            queue.pause()
        };
        if let Err(error) = result {
            tracing::warn!(%guild_id, %error, "failed to toggle pause");
            return None;
        }
        Some(!paused)
    }

    /// 今の曲を飛ばして次へ。飛ばすものがあれば true。
    pub async fn skip(&self, guild_id: GuildId) -> bool {
        let Some(call_lock) = self.songbird.get(guild_id) else {
            return false;
        };
        let call = call_lock.lock().await;
        if call.queue().is_empty() {
            return false;
        }
        match call.queue().skip() {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%guild_id, %error, "failed to skip music");
                false
            }
        }
    }

    /// `/queue` に出る番号（1 = 再生中）でキューから 1 曲取り除く。
    /// 1 を指定した場合は `skip` と同じ（次の曲へ進む）。取り除けたら true。
    pub async fn remove(&self, guild_id: GuildId, position: usize) -> bool {
        if position == 0 {
            return false;
        }
        if position == 1 {
            return self.skip(guild_id).await;
        }

        let Some(call_lock) = self.songbird.get(guild_id) else {
            return false;
        };
        let queued = {
            let call = call_lock.lock().await;
            call.queue().dequeue(position - 1)
        };
        let Some(queued) = queued else {
            return false;
        };

        // songbird のドキュメント通り、キューから外した曲は明示的に stop する。
        let handle = queued.handle();
        if let Err(error) = handle.stop() {
            tracing::warn!(%guild_id, %error, "failed to stop removed track");
        }
        self.tracks.lock().await.remove(&handle.uuid());
        tracing::debug!(%guild_id, position, "music track removed from queue");
        true
    }

    /// 全部止めてキューを空にする。止めるものがあれば true。
    pub async fn stop(&self, guild_id: GuildId) -> bool {
        let Some(call_lock) = self.songbird.get(guild_id) else {
            return false;
        };
        let call = call_lock.lock().await;
        if call.queue().is_empty() {
            return false;
        }
        call.queue().stop();
        drop(call);
        self.tracks.lock().await.clear();
        tracing::debug!(%guild_id, "music queue cleared");
        true
    }

    /// 再生中と待機中の全部に音量を反映する。反映できた件数を返す。
    pub async fn set_volume(&self, guild_id: GuildId, volume: f32) -> usize {
        let Some(call_lock) = self.songbird.get(guild_id) else {
            return 0;
        };
        let handles: Vec<TrackHandle> = {
            let call = call_lock.lock().await;
            call.queue().current_queue()
        };

        handles
            .iter()
            .filter(|handle| handle.set_volume(volume).is_ok())
            .count()
    }

    /// 再生リスト（YouTube の再生リスト／ニコニコのマイリスト・シリーズ）をまとめて積む。
    ///
    /// 曲ごとのタイトル・長さは `--flat-playlist` の一覧情報だけを使う（PLAN §13-12）。
    /// **ただしニコニコだけは例外で、曲ごとに `aux_metadata()` を叩いて再生可否を
    /// 事前確認する**（単曲 `/play` と同じ理由）。ニコニコは会員限定・センシティブ
    /// 指定などで実際には再生できない動画があり、無音のまま飛ばされると理由が
    /// 誰にも伝わらないため。YouTube は今まで通り事前確認しない（`/play` の応答が
    /// リストの曲数分遅れるのを避けるため）。
    pub async fn enqueue_playlist(
        &self,
        guild_id: GuildId,
        url: &str,
        volume: f32,
    ) -> anyhow::Result<PlaylistQueued> {
        let call_lock = self
            .songbird
            .get(guild_id)
            .context("ボイスチャンネルに接続していない")?;

        let is_nico = nicovideo::is_nicovideo(url);
        let entries = list_playlist_entries(url).await?;
        if entries.is_empty() {
            anyhow::bail!("再生リストが空か、取得できませんでした");
        }
        let total = entries.len();

        let mut queued = 0usize;
        let mut unplayable = 0usize;
        for entry in entries.iter().take(PLAYLIST_LIMIT) {
            let Some(track_url) = entry.resolve_url(is_nico) else {
                tracing::warn!(%guild_id, "playlist entry has no resolvable url; skipping");
                continue;
            };

            let (input, info) = if is_nico {
                let mut source = nicovideo::NicoVideo::new(track_url.clone());
                match source.aux_metadata().await {
                    Ok(metadata) => {
                        let info = TrackInfo {
                            title: metadata
                                .title
                                .or_else(|| entry.title.clone())
                                .unwrap_or_else(|| track_url.clone()),
                            duration: metadata.duration,
                            url: metadata.source_url.or(Some(track_url)),
                        };
                        (Input::Lazy(Box::new(source)), info)
                    }
                    Err(error) => {
                        tracing::info!(
                            %guild_id, %error,
                            "nicovideo playlist entry is not playable; skipping"
                        );
                        unplayable += 1;
                        continue;
                    }
                }
            } else {
                let info = TrackInfo {
                    title: entry.title.clone().unwrap_or_else(|| track_url.clone()),
                    duration: entry
                        .duration
                        .filter(|value| value.is_finite() && *value >= 0.0)
                        .map(Duration::from_secs_f64),
                    url: Some(track_url.clone()),
                };
                (
                    Input::from(YoutubeDl::new(self.http.clone(), track_url)),
                    info,
                )
            };

            let handle = {
                let mut call = call_lock.lock().await;
                call.enqueue_input(input).await
            };
            if let Err(error) = handle.set_volume(volume) {
                tracing::warn!(%guild_id, %error, "failed to apply volume");
            }
            self.tracks.lock().await.insert(handle.uuid(), info);
            self.watch_for_end(guild_id, &call_lock, &handle);
            queued += 1;
        }

        tracing::info!(%guild_id, queued, unplayable, total, "playlist queued");
        Ok(PlaylistQueued {
            queued,
            skipped: total.saturating_sub(PLAYLIST_LIMIT),
            unplayable,
        })
    }
}

/// 1 曲の End イベントを受け、次に何が始まったか（尽きたか）を `TrackChanged` にして送る。
/// `watch_for_end` が曲ごとに 1 つずつ生成する。
struct TrackEndAnnouncer {
    guild_id: GuildId,
    uuid: Uuid,
    tracks: Arc<Mutex<HashMap<Uuid, TrackInfo>>>,
    call: Arc<Mutex<Call>>,
    changes: mpsc::UnboundedSender<TrackChanged>,
}

#[async_trait]
impl EventHandler for TrackEndAnnouncer {
    async fn act(&self, _ctx: &EventContext<'_>) -> Option<Event> {
        // `/stop` は tracks を先に空にする。見つからなければ手動停止なので何もしない。
        let finished = self.tracks.lock().await.remove(&self.uuid)?.as_queued();

        let next_handle = self.call.lock().await.queue().current();
        let next = match next_handle {
            Some(handle) => {
                let tracks = self.tracks.lock().await;
                Some(
                    tracks
                        .get(&handle.uuid())
                        .map_or_else(TrackInfo::unknown_track, TrackInfo::as_queued),
                )
            }
            None => None,
        };

        let _ = self.changes.send(TrackChanged {
            guild_id: self.guild_id,
            finished,
            next,
        });
        None
    }
}

/// YouTube の再生リストページ、またはニコニコのマイリスト・シリーズの URL か。
///
/// **プレイリスト専用ページの URL だけを対象にする。** `watch?v=...&list=...` の
/// ように「再生リスト内の 1 曲を再生中」の URL まで拾うと、1 曲だけのつもりで
/// 貼った URL がリスト全体の取り込みになってしまうため。
pub fn is_playlist_url(url: &str) -> bool {
    let Some(host) = nicovideo::host_of(url) else {
        return false;
    };
    let path = url.split(['?', '#']).next().unwrap_or(url);

    if host == "nicovideo.jp" || host.ends_with(".nicovideo.jp") {
        return path.contains("/mylist/") || path.contains("/series/");
    }
    if host == "youtube.com" || host == "www.youtube.com" || host == "m.youtube.com" {
        return path.contains("/playlist") && url.contains("list=");
    }
    false
}

/// `yt-dlp --flat-playlist -j` の 1 行分。yt-dlp のバージョンによって埋まる
/// フィールドが違うので全部 `Option` にし、取れたものだけ使う。
#[derive(serde::Deserialize)]
struct FlatEntry {
    title: Option<String>,
    id: Option<String>,
    /// フラット表示では動画 ID がそのまま入っていることがある。
    url: Option<String>,
    webpage_url: Option<String>,
    /// 秒。一覧の時点で分かることがある（YouTube の一覧ページの表示など）。
    duration: Option<f64>,
}

impl FlatEntry {
    /// 実際に開ける URL を組み立てる。`url` / `webpage_url` が ID のみのことが
    /// あるため、その場合は ID からプラットフォームごとの視聴 URL を組み立て直す。
    fn resolve_url(&self, nicovideo_playlist: bool) -> Option<String> {
        if let Some(webpage) = &self.webpage_url
            && webpage.starts_with("http")
        {
            return Some(webpage.clone());
        }
        if let Some(url) = &self.url
            && url.starts_with("http")
        {
            return Some(url.clone());
        }
        let id = self.id.as_deref().or(self.url.as_deref())?;
        Some(if nicovideo_playlist {
            format!("https://www.nicovideo.jp/watch/{id}")
        } else {
            format!("https://www.youtube.com/watch?v={id}")
        })
    }
}

/// 再生リストの中身を軽く列挙する。曲ごとの詳細は取りに行かない
/// （`--flat-playlist` は一覧ページを読むだけで、動画自体の抽出はしない）。
async fn list_playlist_entries(url: &str) -> anyhow::Result<Vec<FlatEntry>> {
    let mut command = tokio::process::Command::new(YTDLP);
    command.args(["--flat-playlist", "-j", "--no-warnings"]);
    // ニコニコの会員限定マイリストなどは Cookie が要る。個別動画と同じ仕組みを使う。
    let jar = nicovideo::cookie_copy();
    if let Some(path) = jar.as_ref().and_then(CookieCopy::path) {
        command.args(["--cookies", path]);
    }
    command.arg(url);

    let output = command
        .stdin(Stdio::null())
        .output()
        .await
        .context("yt-dlp の起動に失敗した")?;

    if !output.status.success() {
        let reason = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "再生リストを取得できませんでした: {}",
            nicovideo::readable_error(&reason)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

/// タイトルと長さを引く。取れなくても再生は続ける（yt-dlp をもう一度叩くので失敗しうる）。
///
/// ここで取った長さをそのまま持っておく。あとで進捗バーを描くたびに
/// yt-dlp を叩き直さずに済ませたいため。
async fn describe(source: &mut dyn Compose, guild_id: GuildId, fallback: &str) -> TrackInfo {
    match source.aux_metadata().await {
        Ok(metadata) => TrackInfo {
            title: metadata.title.unwrap_or_else(|| fallback.to_owned()),
            duration: metadata.duration,
            // `source_url` は yt-dlp の `webpage_url`。検索で入れた曲でも、
            // 実際に選ばれた動画のページが入る。
            url: metadata.source_url,
        },
        Err(error) => {
            tracing::debug!(%guild_id, %error, "failed to fetch metadata");
            TrackInfo {
                title: fallback.to_owned(),
                duration: None,
                url: None,
            }
        }
    }
}

/// 再生中トラックの状態を見る。取れないときは「止まっていない」扱いにする。
async fn is_paused(queue: &songbird::tracks::TrackQueue) -> bool {
    let Some(current) = queue.current() else {
        return false;
    };
    match current.get_info().await {
        Ok(state) => state.playing == songbird::tracks::PlayMode::Pause,
        Err(_) => false,
    }
}

/// 曲名を押すと元のページへ飛べる形にする。URL が無ければ曲名だけを返す。
///
/// 曲名は他人が付けたものなので `[` `]` や `*` が入りうる。素で埋めると
/// リンクが壊れたり、意図しない太字・打ち消し線になったりする。必ずエスケープする。
///
/// URL は `<...>` で囲む。**囲まないと Discord がリンク先のプレビューを展開し、
/// `/queue` のように何曲も並ぶところが埋め込みだらけになる。** poise の
/// `CreateReply` には `SUPPRESS_EMBEDS` を立てる口が無いので、ここで抑える。
pub fn track_link(title: &str, url: Option<&str>) -> String {
    let label = escape_markdown(title);
    match url.filter(|url| is_safe_link(url)) {
        Some(url) => format!("[{label}](<{url}>)"),
        None => label,
    }
}

/// Discord の書式指定に使われる文字を打ち消す。
fn escape_markdown(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(
            character,
            '\\' | '*' | '_' | '~' | '`' | '|' | '[' | ']' | '<' | '>'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// `(...)` を壊さずに埋められる URL か。
///
/// 壊すものは**エスケープせずにリンクをやめる**。中途半端に組み立てて
/// 崩れた表示になるより、曲名だけ出すほうがましなため。
fn is_safe_link(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://"))
        && !url.contains(['(', ')', ' ', '\n', '\t', '<', '>'])
}

/// 進捗バーの目盛りの数。スマホの幅で折り返さない程度に抑えている。
const BAR_CELLS: usize = 15;

/// `▬▬▬◉▬▬▬  2:14 / 4:52` を作る。
///
/// 長さが分からない曲（ライブ配信など）はバーが嘘になるので、経過時間だけ出す。
pub fn progress_bar(position: Duration, total: Option<Duration>) -> String {
    let Some(total) = total.filter(|total| !total.is_zero()) else {
        return format!("{}（長さ不明）", format_time(position));
    };

    // 位置が長さを超えることがある（シーク直後や yt-dlp のメタデータのずれ）。
    let position = position.min(total);
    let ratio = position.as_secs_f64() / total.as_secs_f64();
    // 最後の目盛りに乗るのは本当に終端まで来たときだけにする。
    let filled = ((ratio * BAR_CELLS as f64) as usize).min(BAR_CELLS - 1);

    let bar: String = (0..BAR_CELLS)
        .map(|index| if index == filled { '◉' } else { '▬' })
        .collect();
    format!("{bar}  {} / {}", format_time(position), format_time(total))
}

/// `m:ss` 形式。1 時間を超えるものだけ `h:mm:ss` にする。
pub fn format_time(value: Duration) -> String {
    let total = value.as_secs();
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(value: u64) -> Duration {
        Duration::from_secs(value)
    }

    fn entry(id: Option<&str>, url: Option<&str>, webpage_url: Option<&str>) -> FlatEntry {
        FlatEntry {
            title: None,
            id: id.map(str::to_owned),
            url: url.map(str::to_owned),
            webpage_url: webpage_url.map(str::to_owned),
            duration: None,
        }
    }

    #[test]
    fn youtube_playlist_pages_are_recognised() {
        assert!(is_playlist_url(
            "https://www.youtube.com/playlist?list=PLxxxx"
        ));
        assert!(is_playlist_url(
            "https://m.youtube.com/playlist?list=PLxxxx"
        ));
    }

    /// 再生リスト内の 1 曲を再生中の URL。これは「この曲だけ」の意図とみなす。
    #[test]
    fn youtube_watch_urls_with_a_list_param_are_not_playlists() {
        assert!(!is_playlist_url(
            "https://www.youtube.com/watch?v=abc&list=PLxxxx"
        ));
    }

    /// youtu.be の短縮 URL は常に単曲。
    #[test]
    fn youtu_be_short_links_are_never_playlists() {
        assert!(!is_playlist_url("https://youtu.be/abc?list=PLxxxx"));
    }

    #[test]
    fn nicovideo_mylist_and_series_are_recognised() {
        assert!(is_playlist_url("https://www.nicovideo.jp/mylist/12345"));
        assert!(is_playlist_url("https://www.nicovideo.jp/series/12345"));
    }

    #[test]
    fn nicovideo_watch_urls_are_not_playlists() {
        assert!(!is_playlist_url("https://www.nicovideo.jp/watch/sm9"));
    }

    #[test]
    fn search_terms_are_not_playlists() {
        assert!(!is_playlist_url("プレイリスト"));
        assert!(!is_playlist_url(""));
    }

    #[test]
    fn resolve_url_prefers_webpage_url() {
        let entry = entry(Some("x"), Some("y"), Some("https://example.com/w"));
        assert_eq!(
            entry.resolve_url(false),
            Some("https://example.com/w".to_owned())
        );
    }

    #[test]
    fn resolve_url_falls_back_to_a_full_url_field() {
        let entry = entry(Some("x"), Some("https://example.com/u"), None);
        assert_eq!(
            entry.resolve_url(false),
            Some("https://example.com/u".to_owned())
        );
    }

    #[test]
    fn resolve_url_builds_a_youtube_watch_url_from_the_id() {
        let entry = entry(Some("abc123"), Some("abc123"), None);
        assert_eq!(
            entry.resolve_url(false),
            Some("https://www.youtube.com/watch?v=abc123".to_owned())
        );
    }

    #[test]
    fn resolve_url_builds_a_nicovideo_watch_url_from_the_id() {
        let entry = entry(Some("sm9"), None, None);
        assert_eq!(
            entry.resolve_url(true),
            Some("https://www.nicovideo.jp/watch/sm9".to_owned())
        );
    }

    #[test]
    fn resolve_url_is_none_without_any_identifier() {
        let entry = entry(None, None, None);
        assert_eq!(entry.resolve_url(false), None);
    }

    /// バーの本体（時刻より前の部分）だけ取り出す。
    fn cells(rendered: &str) -> Vec<char> {
        rendered
            .chars()
            .take_while(|c| *c == '▬' || *c == '◉')
            .collect()
    }

    #[test]
    fn title_links_to_its_page() {
        assert_eq!(
            track_link("曲名", Some("https://youtu.be/abc")),
            "[曲名](<https://youtu.be/abc>)"
        );
    }

    /// URL を `<...>` で囲まないと、`/queue` が埋め込みだらけになる。
    #[test]
    fn the_url_is_wrapped_to_suppress_the_preview() {
        let rendered = track_link("曲名", Some("https://youtu.be/abc"));
        assert!(rendered.contains("(<https://"), "{rendered}");
        assert!(rendered.ends_with(">)"), "{rendered}");
    }

    /// URL が取れなかった曲は曲名だけ出す。
    #[test]
    fn title_without_a_url_stays_plain() {
        assert_eq!(track_link("曲名", None), "曲名");
    }

    /// 角括弧入りの曲名でリンクが壊れないこと。ここが本題。
    #[test]
    fn brackets_in_the_title_are_escaped() {
        assert_eq!(
            track_link("[MV] 曲名", Some("https://youtu.be/abc")),
            "[\\[MV\\] 曲名](<https://youtu.be/abc>)"
        );
    }

    /// 曲名に含まれる記号で勝手に書式が付かないこと。
    #[test]
    fn markdown_in_the_title_is_escaped() {
        assert_eq!(
            track_link("*強調* _した_ `名前`", None),
            "\\*強調\\* \\_した\\_ \\`名前\\`"
        );
        assert_eq!(track_link("~~消し~~", None), "\\~\\~消し\\~\\~");
        assert_eq!(track_link("a|b", None), "a\\|b");
        assert_eq!(track_link("back\\slash", None), "back\\\\slash");
    }

    /// `<...>` を素通しすると、`<@123>` のような曲名がメンションとして解釈される。
    #[test]
    fn angle_brackets_in_the_title_are_escaped() {
        assert_eq!(track_link("<@123>", None), "\\<@123\\>");
    }

    /// 括弧を含む URL はリンクを壊すので、リンクにしない。
    #[test]
    fn urls_that_would_break_the_link_are_dropped() {
        assert_eq!(track_link("曲名", Some("https://e.com/a(b)c")), "曲名");
        assert_eq!(track_link("曲名", Some("https://e.com/a b")), "曲名");
    }

    /// http(s) 以外はリンクにしない。
    #[test]
    fn non_http_urls_are_not_linked() {
        assert_eq!(track_link("曲名", Some("javascript:alert(1)")), "曲名");
        assert_eq!(track_link("曲名", Some("ftp://e.com/a")), "曲名");
        assert_eq!(track_link("曲名", Some("")), "曲名");
    }

    #[test]
    fn bar_always_has_a_fixed_number_of_cells() {
        for position in [0, 1, 60, 145, 291, 292, 1000] {
            let rendered = progress_bar(secs(position), Some(secs(292)));
            assert_eq!(cells(&rendered).len(), BAR_CELLS, "position={position}");
        }
    }

    #[test]
    fn knob_starts_at_the_left() {
        let rendered = progress_bar(secs(0), Some(secs(292)));
        assert_eq!(cells(&rendered).iter().position(|c| *c == '◉'), Some(0));
        assert!(rendered.ends_with("0:00 / 4:52"), "{rendered}");
    }

    #[test]
    fn knob_moves_with_the_position() {
        let rendered = progress_bar(secs(146), Some(secs(292)));
        // ちょうど半分なので 15 目盛りの 7 番目（0 起算）に乗る。
        assert_eq!(cells(&rendered).iter().position(|c| *c == '◉'), Some(7));
        assert!(rendered.ends_with("2:26 / 4:52"), "{rendered}");
    }

    #[test]
    fn knob_stops_at_the_last_cell() {
        let rendered = progress_bar(secs(292), Some(secs(292)));
        assert_eq!(
            cells(&rendered).iter().position(|c| *c == '◉'),
            Some(BAR_CELLS - 1)
        );
    }

    /// 位置が長さを超えても描けること。はみ出した位置は表示上も丸める。
    #[test]
    fn position_beyond_the_end_is_clamped() {
        let rendered = progress_bar(secs(400), Some(secs(292)));
        assert_eq!(
            cells(&rendered).iter().position(|c| *c == '◉'),
            Some(BAR_CELLS - 1)
        );
        assert!(rendered.ends_with("4:52 / 4:52"), "{rendered}");
    }

    /// 長さ 0 で割り算しないこと。
    #[test]
    fn zero_length_falls_back_to_elapsed_only() {
        let rendered = progress_bar(secs(10), Some(secs(0)));
        assert_eq!(rendered, "0:10（長さ不明）");
    }

    #[test]
    fn unknown_length_falls_back_to_elapsed_only() {
        let rendered = progress_bar(secs(75), None);
        assert_eq!(rendered, "1:15（長さ不明）");
        assert!(cells(&rendered).is_empty());
    }

    #[test]
    fn time_is_formatted_as_minutes_and_seconds() {
        assert_eq!(format_time(secs(0)), "0:00");
        assert_eq!(format_time(secs(9)), "0:09");
        assert_eq!(format_time(secs(74)), "1:14");
        assert_eq!(format_time(secs(292)), "4:52");
        assert_eq!(format_time(secs(599)), "9:59");
    }

    /// 1 時間を超えたら時も出す（長い配信アーカイブ）。
    #[test]
    fn hours_are_shown_only_when_needed() {
        assert_eq!(format_time(secs(3599)), "59:59");
        assert_eq!(format_time(secs(3600)), "1:00:00");
        assert_eq!(format_time(secs(3661)), "1:01:01");
        assert_eq!(format_time(secs(7325)), "2:02:05");
    }
}
