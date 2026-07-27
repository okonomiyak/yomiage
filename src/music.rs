//! 音楽再生（yt-dlp）。
//!
//! 読み上げとは別のトラックとして流す。songbird は 1 つの Call に複数トラックを
//! 混ぜられるので、音楽の上に読み上げが乗る形になる。音量を既定で小さめにしてあるのはそのため。
//!
//! キューは songbird の `builtin-queue` に任せる。自前で持つと再生完了の検知と
//! 状態の同期を自分で書くことになり、ずれの原因になる。読み上げは `play_input` で
//! 直接鳴らしているのでキューには入らず、音楽とは独立して動く。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use poise::serenity_prelude::GuildId;
use songbird::Songbird;
use songbird::input::{Compose, Input, YoutubeDl};
use songbird::tracks::TrackHandle;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::nicovideo;

/// タイトルも長さも取れなかった曲の表示。
const UNKNOWN_TITLE: &str = "（タイトル不明）";

/// シークで曲の終端に着地しないよう手前に残す余白。
/// 終端ちょうどへ飛ばすと、シークが終わった瞬間に曲が終わる。
const SEEK_TAIL_MARGIN: Duration = Duration::from_secs(3);

pub struct Manager {
    songbird: Arc<Songbird>,
    /// yt-dlp が返したストリーム URL を取りに行くのに使う。
    http: reqwest::Client,
    /// トラック UUID → 曲の情報。songbird 0.6 の `TrackHandle::data` は型が違うと
    /// panic するので使わない（読み上げのトラックと混ざる余地を残さない）。
    tracks: Mutex<HashMap<Uuid, TrackInfo>>,
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

/// 今流れている曲の状態。パネルの描画に要るものだけ。
pub struct NowPlaying {
    pub track: QueuedTrack,
    pub position: Duration,
    /// 取れないことがある（ライブ配信など）。その場合はバーを出さない。
    pub duration: Option<Duration>,
    pub paused: bool,
}

impl Manager {
    pub fn new(songbird: Arc<Songbird>, http: reqwest::Client) -> Self {
        Self {
            songbird,
            http,
            tracks: Mutex::new(HashMap::new()),
        }
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
        Ok(Queued { track, position })
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
