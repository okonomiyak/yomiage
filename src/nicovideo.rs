//! ニコニコ動画の再生。
//!
//! # なぜ songbird の `YoutubeDl` をそのまま使えないのか
//!
//! `YoutubeDl` は `yt-dlp -j` でストリーム URL とヘッダだけ取り出し、**取得は
//! songbird 自身が HTTP で行う**。ところがニコニコの配信（domand）は、抽出のときに
//! yt-dlp が確立する `domand_bid` という Cookie を要求する。この Cookie は
//! yt-dlp が返す JSON の `http_headers` に**含まれない**ため、songbird が同じ URL を
//! 取りに行くと 403 になる。
//!
//! そこで、ニコニコだけは **yt-dlp 自身にダウンロードさせて、その標準出力を
//! そのまま songbird に流す**。Cookie は yt-dlp の中で完結するので問題にならない。
//!
//! 音声は `audio-aac-*`（HLS / fMP4 + AAC）が選ばれる。`Cargo.toml` で symphonia の
//! `isomp4` と `aac` を有効にしてあるのでデコードできる。**どちらかを外すと、
//! 取得は成功するのに音が出なくなる**。回帰テストは `tests` を参照。

use std::process::{Command, Stdio};
use std::time::Duration;

use poise::serenity_prelude::async_trait;
use songbird::input::core::io::ReadOnlySource;
use songbird::input::{
    AudioStream, AudioStreamError, AuxMetadata, ChildContainer, Compose, core::io::MediaSource,
};

/// songbird が使うものと同じ実行ファイル名。
const YTDLP: &str = "yt-dlp";

/// 音声のみで、ビットレートが分かっているものを優先する。
/// songbird の `YoutubeDl` と同じ指定にして、挙動を合わせておく。
const FORMAT: &str = "ba[abr>0][vcodec=none]/best";

/// Cookie ファイルの場所を持つ環境変数。
const COOKIES_ENV: &str = "NICO_COOKIES";

/// ログイン済みの Cookie ファイル（Netscape 形式）を探す。
///
/// 会員限定・年齢制限・センシティブ指定の動画はログイン状態が要る。
/// **未設定でも普通に動く**（公開動画だけ再生できる）ので、無ければ黙って諦める。
///
/// # 置き場所について
///
/// **書き込み可能な場所に置くこと。** yt-dlp は終了時に必ず Cookie を書き戻すので、
/// 読み取り専用でマウントすると `OSError: Read-only file system` で落ち、
/// **抽出に成功していても終了コードが 1 になる**（実機で確認済み）。
/// 書き戻しはセッションの更新でもあるので、置いたままにしておくと期限が延びる。
fn cookies() -> Option<String> {
    let path = std::env::var(COOKIES_ENV).ok()?;
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    // 置き忘れ・パスの打ち間違いで毎回落ちるより、認証なしで動くほうがよい。
    if !std::path::Path::new(path).is_file() {
        tracing::warn!(
            path,
            "{COOKIES_ENV} の指す先にファイルが無い; 認証なしで続行する"
        );
        return None;
    }
    Some(path.to_owned())
}

/// ニコニコ動画の 1 本。
///
/// `Compose` にしてあるので、**再生が始まる直前まで yt-dlp を起動しない**。
/// キューの後ろに積まれた曲がいきなりダウンロードを始めてパイプを詰まらせるのを防ぐ。
/// 巻き戻しのときは songbird がこれを使って取り直す。
pub struct NicoVideo {
    url: String,
    /// 一度引いたメタデータは使い回す。yt-dlp の起動は数秒かかる。
    metadata: Option<AuxMetadata>,
}

impl NicoVideo {
    pub fn new(url: String) -> Self {
        Self {
            url,
            metadata: None,
        }
    }
}

#[async_trait]
impl Compose for NicoVideo {
    /// 同期版は使わない（`should_create_async` が true）。songbird の `YoutubeDl` と同じ。
    fn create(&mut self) -> Result<AudioStream<Box<dyn MediaSource>>, AudioStreamError> {
        Err(AudioStreamError::Unsupported)
    }

    async fn create_async(
        &mut self,
    ) -> Result<AudioStream<Box<dyn MediaSource>>, AudioStreamError> {
        let mut command = Command::new(YTDLP);
        command.args([
            "--no-playlist",
            "--quiet",
            "--no-warnings",
            "--no-progress",
            "-f",
            FORMAT,
        ]);
        if let Some(path) = cookies() {
            command.args(["--cookies", &path]);
        }
        // 標準出力へ流す。ここがこのモジュールの肝。
        command.args(["-o", "-", &self.url]);

        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            // 進捗も警告も読まないので捨てる。失敗したら stdout が空になり、
            // symphonia がデコードに失敗して 1 曲飛ぶだけで済む。
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                AudioStreamError::Fail(if error.kind() == std::io::ErrorKind::NotFound {
                    format!("'{YTDLP}' が見つからない").into()
                } else {
                    Box::new(error)
                })
            })?;

        tracing::debug!(url = self.url.as_str(), "spawned yt-dlp for nicovideo");

        // ChildContainer は drop でプロセスを確実に殺す。/skip や /stop で
        // 再生をやめたときに yt-dlp が残らない。
        let container = ChildContainer::from(child);
        Ok(AudioStream {
            input: Box::new(ReadOnlySource::new(container)) as Box<dyn MediaSource>,
        })
    }

    fn should_create_async(&self) -> bool {
        true
    }

    async fn aux_metadata(&mut self) -> Result<AuxMetadata, AudioStreamError> {
        if let Some(metadata) = &self.metadata {
            return Ok(metadata.clone());
        }

        // ここは待つので tokio 側の Command を使う。std::process の output() だと
        // yt-dlp の抽出が終わるまでワーカースレッドを止めてしまう。
        let mut command = tokio::process::Command::new(YTDLP);
        command.args(["-j", "--no-playlist", "--no-warnings"]);
        if let Some(path) = cookies() {
            command.args(["--cookies", &path]);
        }
        command.arg(&self.url);

        let output = command
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|error| {
                AudioStreamError::Fail(if error.kind() == std::io::ErrorKind::NotFound {
                    format!("'{YTDLP}' が見つからない").into()
                } else {
                    Box::new(error)
                })
            })?;

        if !output.status.success() {
            let reason = String::from_utf8_lossy(&output.stderr);
            return Err(AudioStreamError::Fail(
                format!("yt-dlp が失敗した: {reason}").into(),
            ));
        }

        let probe: Probe = serde_json::from_slice(&output.stdout)
            .map_err(|error| AudioStreamError::Fail(Box::new(error)))?;

        let metadata = AuxMetadata {
            title: probe.title,
            duration: probe.duration.map(Duration::from_secs_f64),
            source_url: Some(self.url.clone()),
            ..AuxMetadata::default()
        };
        self.metadata = Some(metadata.clone());
        Ok(metadata)
    }
}

/// `yt-dlp -j` の出力から、使うものだけ拾う。
#[derive(serde::Deserialize)]
struct Probe {
    title: Option<String>,
    /// 秒。ニコニコでは基本取れるが、取れなければシークバーを出さないだけ。
    duration: Option<f64>,
}

/// yt-dlp のエラー文を、そのまま Discord に出せる長さへ縮める。
///
/// yt-dlp は `ERROR: [niconico] sm123: 理由. Use --cookies ... See https://...` の
/// ような長文を返す。そのまま貼るとチャンネルが荒れるので、理由だけ取り出す。
pub fn readable_error(raw: &str) -> String {
    // 最後の `ERROR:` 以降が本体。
    let body = raw.rsplit("ERROR:").next().unwrap_or(raw).trim();
    // 対処法の案内は利用者には関係ないので落とす。
    let body = body
        .split("Use --cookies")
        .next()
        .unwrap_or(body)
        .trim()
        .trim_end_matches('.');
    // `[niconico] ` と、続く `sm123: ` の前置きを落とす。
    let body = body
        .strip_prefix('[')
        .and_then(|rest| rest.split_once("] "))
        .map_or(body, |(_, rest)| rest);
    let body = body.split_once(": ").map_or(body, |(_, rest)| rest);

    // 一番よく当たるものは日本語にして、対処が分かるようにする。
    if body.contains("login required") || body.contains("Sensitive content") {
        return "ログインが必要な動画のため再生できません（年齢制限やセンシティブ指定など）"
            .to_owned();
    }

    // それでも長いものは切る。
    const LIMIT: usize = 150;
    if body.chars().count() > LIMIT {
        let short: String = body.chars().take(LIMIT).collect();
        format!("{short}…")
    } else {
        body.to_owned()
    }
}

/// ニコニコ動画の URL か。
///
/// **ホストだけを見る。** 文字列に `nicovideo.jp` が含まれるかで判定すると、
/// `https://example.com/?q=nicovideo.jp` のような URL を巻き込む。
pub fn is_nicovideo(query: &str) -> bool {
    let Some(host) = host_of(query) else {
        return false;
    };
    host == "nicovideo.jp" || host.ends_with(".nicovideo.jp") || host == "nico.ms"
}

/// URL からホスト部分だけを小文字で取り出す。http(s) 以外は None。
fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;

    // パス・クエリ・フラグメントを落とす。
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|value| !value.is_empty())?;

    // 認証情報とポートを落とす。
    let host = authority.rsplit('@').next()?;
    let host = match host.rsplit_once(':') {
        // IPv6 は今回相手にしないので、`]` を含むならポートではない。
        Some((left, _)) if !left.ends_with(']') => left,
        _ => host,
    };

    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 実際に本番で出た文言。長い対処法の案内をそのまま貼らないこと。
    #[test]
    fn login_required_becomes_a_short_japanese_message() {
        let raw = "failed to create audio: yt-dlp が失敗した: ERROR: [niconico] \
                   sm43138635: Sensitive content, login required. Use --cookies, \
                   --cookies-from-browser, --username and --password, --netrc-cmd, \
                   or --netrc (niconico) to provide account credentials. See \
                   https://github.com/yt-dlp/yt-dlp/wiki/FAQ#how-do-i-pass-cookies-to-yt-dlp";
        assert_eq!(
            readable_error(raw),
            "ログインが必要な動画のため再生できません（年齢制限やセンシティブ指定など）"
        );
    }

    /// 前置きを落として理由だけ残す。
    #[test]
    fn the_extractor_prefix_is_dropped() {
        let raw = "ERROR: [niconico] sm1: Video unavailable";
        assert_eq!(readable_error(raw), "Video unavailable");
    }

    /// 前置きが無い文はそのまま通す。
    #[test]
    fn a_plain_message_is_kept() {
        assert_eq!(readable_error("Video unavailable"), "Video unavailable");
    }

    /// 長すぎるものは切る。Discord に長文を吐かないため。
    #[test]
    fn a_long_message_is_truncated() {
        let raw = format!("ERROR: [niconico] sm1: {}", "あ".repeat(400));
        let shortened = readable_error(&raw);
        assert!(shortened.chars().count() <= 151, "{shortened}");
        assert!(shortened.ends_with('…'), "{shortened}");
    }

    #[test]
    fn watch_urls_are_recognised() {
        assert!(is_nicovideo("https://www.nicovideo.jp/watch/sm9"));
        assert!(is_nicovideo("http://www.nicovideo.jp/watch/sm9"));
        assert!(is_nicovideo("https://nicovideo.jp/watch/sm9"));
        assert!(is_nicovideo("https://sp.nicovideo.jp/watch/sm9"));
    }

    /// 短縮 URL も同じ扱いにする。
    #[test]
    fn short_urls_are_recognised() {
        assert!(is_nicovideo("https://nico.ms/sm9"));
    }

    #[test]
    fn query_and_fragment_do_not_confuse_the_host() {
        assert!(is_nicovideo("https://www.nicovideo.jp/watch/sm9?ref=x"));
        assert!(is_nicovideo("https://www.nicovideo.jp/watch/sm9#t=30"));
    }

    /// ホストだけを見ること。ここが緩いと他所の URL をニコニコ扱いしてしまう。
    #[test]
    fn other_hosts_are_not_nicovideo() {
        assert!(!is_nicovideo("https://www.youtube.com/watch?v=abc"));
        assert!(!is_nicovideo("https://example.com/?q=nicovideo.jp"));
        assert!(!is_nicovideo("https://example.com/nicovideo.jp/watch/sm9"));
        assert!(!is_nicovideo("https://nicovideo.jp.example.com/watch/sm9"));
        assert!(!is_nicovideo("https://notnicovideo.jp/watch/sm9"));
    }

    /// URL でないもの（検索語）は YouTube 検索に回す。
    #[test]
    fn search_terms_are_not_urls() {
        assert!(!is_nicovideo("ニコニコ動画"));
        assert!(!is_nicovideo("nicovideo.jp"));
        assert!(!is_nicovideo(""));
    }

    #[test]
    fn port_and_userinfo_are_stripped() {
        assert!(is_nicovideo("https://www.nicovideo.jp:443/watch/sm9"));
        assert!(is_nicovideo("https://user@www.nicovideo.jp/watch/sm9"));
    }

    /// **ニコニコの音声が songbird で実際に再生可能になるか。**
    ///
    /// symphonia の `isomp4` / `aac` を落とすとここで落ちる。取得は成功するのに
    /// 音だけ出ない、という一番見つけにくい壊れ方を捕まえるためのテスト。
    #[tokio::test]
    #[ignore = "ネットワークと yt-dlp が必要"]
    async fn nicovideo_stream_is_playable_by_songbird() {
        use songbird::input::Input;
        use songbird::input::codecs::{get_codec_registry, get_probe};

        let mut source = NicoVideo::new("https://www.nicovideo.jp/watch/sm9".to_owned());

        let metadata = source.aux_metadata().await.expect("メタデータが取れない");
        assert!(
            metadata.duration.is_some(),
            "長さが取れないとバーが出せない"
        );

        let input = Input::Lazy(Box::new(source));
        let playable = input
            .make_playable_async(get_codec_registry(), get_probe())
            .await;
        assert!(playable.is_ok(), "songbird がニコニコの音声を再生できない");
    }
}
