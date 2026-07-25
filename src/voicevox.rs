//! VOICEVOX ENGINE クライアント（PLAN §8）。
//!
//! この層のエラーは `thiserror` で独自型にする。アプリ層で `anyhow` に載せ替える。

use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// AudioQuery で指定する出力形式。Discord は 48kHz ステレオなので、
/// ENGINE 側で合わせておくと Discord 側でのリサンプリングを省ける（PLAN §7-5）。
pub const OUTPUT_SAMPLING_RATE: u32 = 48_000;
/// 48kHz / ステレオ / 16bit の 1 秒あたりバイト数。再生時間の見積もりに使う。
pub const BYTES_PER_SEC: u32 = OUTPUT_SAMPLING_RATE * 2 * 2;

/// 既定の話者。ずんだもん（ノーマル）のスタイル ID（PLAN §9）。
pub const DEFAULT_STYLE: StyleId = StyleId(3);

/// ENGINE の `speaker` パラメータに渡すのは**キャラ ID ではなくスタイル ID**
/// （`/speakers` の `styles[].id`）。混同すると別人の声になるので newtype で区別する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StyleId(pub u32);

impl std::fmt::Display for StyleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// ENGINE が受け付けるパラメータの範囲（VOICEVOX エディタの UI と同じ）。
pub const SPEED_RANGE: std::ops::RangeInclusive<f32> = 0.5..=2.0;
pub const PITCH_RANGE: std::ops::RangeInclusive<f32> = -0.15..=0.15;
pub const INTONATION_RANGE: std::ops::RangeInclusive<f32> = 0.0..=2.0;

/// ユーザーごとの声の設定（PLAN §9 の `user_voice`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Voice {
    pub style: StyleId,
    pub speed: f32,
    pub pitch: f32,
    pub intonation: f32,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            style: DEFAULT_STYLE,
            speed: 1.0,
            pitch: 0.0,
            intonation: 1.0,
        }
    }
}

/// `/speakers` の 1 要素。キャラクターが複数のスタイルを持つ。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Speaker {
    pub name: String,
    pub styles: Vec<Style>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Style {
    pub name: String,
    pub id: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("ENGINE のベース URL が不正: {0}")]
    BaseUrl(String),
    #[error("ENGINE への HTTP リクエストが失敗した")]
    Http(#[from] reqwest::Error),
    #[error("ENGINE が {status} を返した: {body}")]
    Status {
        status: reqwest::StatusCode,
        body: String,
    },
}

pub struct Client {
    http: reqwest::Client,
    /// 末尾スラッシュを落としたベース URL。
    base: String,
}

impl Client {
    pub fn new(base: &str) -> Result<Self, Error> {
        let base = base.trim_end_matches('/').to_owned();
        if !base.starts_with("http://") && !base.starts_with("https://") {
            return Err(Error::BaseUrl(base));
        }
        let http = reqwest::Client::builder()
            // 上限付近の長文でも 5 秒程度（PLAN §4.1）。詰まったときに永久に待たない。
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self { http, base })
    }

    /// ヘルスチェック。ENGINE のバージョン文字列を返す。
    pub async fn version(&self) -> Result<String, Error> {
        let res = self
            .http
            .get(format!("{}/version", self.base))
            .send()
            .await?;
        // /version は JSON 文字列（"0.25.2"）を返すので引用符を落とす。
        let version = check(res).await?.text().await?;
        Ok(version.trim().trim_matches('"').to_owned())
    }

    /// 話者のウォームアップ。起動時に叩いておかないと初回発話がモデルロードで待たされる（PLAN §7 補足）。
    pub async fn initialize_speaker(&self, style: StyleId) -> Result<(), Error> {
        let res = self
            .http
            .post(format!("{}/initialize_speaker", self.base))
            .query(&[("speaker", style.0)])
            .send()
            .await?;
        check(res).await?;
        Ok(())
    }

    /// 話者一覧。`/voice` のオートコンプリート用に呼び出し側でキャッシュする（PLAN §8）。
    pub async fn speakers(&self) -> Result<Vec<Speaker>, Error> {
        let res = self
            .http
            .get(format!("{}/speakers", self.base))
            .send()
            .await?;
        Ok(check(res).await?.json().await?)
    }

    /// テキスト → wav（48kHz ステレオ）。
    pub async fn synthesize(&self, text: &str, voice: Voice) -> Result<Vec<u8>, Error> {
        let started = Instant::now();

        let mut query = self.audio_query(text, voice.style).await?;
        if let Some(object) = query.as_object_mut() {
            // ユーザー設定で上書きする（PLAN §7-5）。
            object.insert("speedScale".to_owned(), json!(voice.speed));
            object.insert("pitchScale".to_owned(), json!(voice.pitch));
            object.insert("intonationScale".to_owned(), json!(voice.intonation));
            object.insert("outputSamplingRate".to_owned(), json!(OUTPUT_SAMPLING_RATE));
            object.insert("outputStereo".to_owned(), json!(true));
        }
        let wav = self.synthesis(&query, voice.style).await?;

        tracing::debug!(
            style = %voice.style,
            chars = text.chars().count(),
            bytes = wav.len(),
            latency_ms = started.elapsed().as_millis(),
            "synthesized",
        );
        Ok(wav)
    }

    /// AudioQuery は ENGINE のバージョンでフィールドが増減するので、型を起こさず Value のまま扱う。
    async fn audio_query(&self, text: &str, style: StyleId) -> Result<Value, Error> {
        // クエリ文字列は reqwest に組み立てさせる。手で URL に埋めると
        // 日本語が未エンコードのまま送られ、ENGINE 側の uvicorn に弾かれる。
        let res = self
            .http
            .post(format!("{}/audio_query", self.base))
            .query(&[("text", text), ("speaker", &style.0.to_string())])
            .send()
            .await?;
        Ok(check(res).await?.json().await?)
    }

    async fn synthesis(&self, query: &Value, style: StyleId) -> Result<Vec<u8>, Error> {
        let res = self
            .http
            .post(format!("{}/synthesis", self.base))
            .query(&[("speaker", style.0)])
            .json(query)
            .send()
            .await?;
        Ok(check(res).await?.bytes().await?.to_vec())
    }
}

async fn check(res: reqwest::Response) -> Result<reqwest::Response, Error> {
    let status = res.status();
    if status.is_success() {
        return Ok(res);
    }
    // エラー本文はそのまま流すと長いので頭だけ残す。
    let body: String = res
        .text()
        .await
        .unwrap_or_else(|_| "<本文の読み取りに失敗>".to_owned())
        .chars()
        .take(200)
        .collect();
    Err(Error::Status { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_client() -> Client {
        let base =
            std::env::var("VOICEVOX_URL").unwrap_or_else(|_| "http://localhost:50021".to_owned());
        Client::new(&base).expect("テスト用の URL が不正")
    }

    #[test]
    fn base_url_must_be_http() {
        assert!(matches!(
            Client::new("localhost:50021"),
            Err(Error::BaseUrl(_))
        ));
        assert!(Client::new("http://localhost:50021/").is_ok());
    }

    // 以下は ENGINE を立てて実行する（PLAN の方針どおり HTTP はモックしない）。
    //   docker compose -f compose.yaml -f compose.dev.yaml up -d voicevox
    //   cargo test -- --ignored

    #[tokio::test]
    #[ignore = "ENGINE の起動が必要"]
    async fn version_returns_something() {
        let version = test_client().version().await.expect("/version に失敗");
        assert!(!version.is_empty());
    }

    #[tokio::test]
    #[ignore = "ENGINE の起動が必要"]
    async fn speakers_include_default_style() {
        let speakers = test_client().speakers().await.expect("/speakers に失敗");
        let found = speakers
            .iter()
            .flat_map(|speaker| &speaker.styles)
            .any(|style| style.id == DEFAULT_STYLE.0);
        assert!(found, "既定スタイル {DEFAULT_STYLE} が一覧に無い");
    }

    #[tokio::test]
    #[ignore = "ENGINE の起動が必要"]
    async fn synthesize_returns_48khz_stereo_wav() {
        let client = test_client();
        client
            .initialize_speaker(DEFAULT_STYLE)
            .await
            .expect("/initialize_speaker に失敗");
        let wav = client
            .synthesize("これはテストなのだ。", Voice::default())
            .await
            .expect("合成に失敗");

        assert_eq!(&wav[0..4], b"RIFF", "wav ヘッダが無い");
        // fmt チャンク: 22-23 = チャンネル数, 24-27 = サンプリングレート
        let channels = u16::from_le_bytes([wav[22], wav[23]]);
        let rate = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
        assert_eq!(channels, 2, "ステレオになっていない");
        assert_eq!(rate, OUTPUT_SAMPLING_RATE, "48kHz になっていない");
    }

    #[tokio::test]
    #[ignore = "ENGINE の起動が必要"]
    async fn unknown_style_is_reported_as_status_error() {
        let voice = Voice {
            style: StyleId(99_999),
            ..Voice::default()
        };
        let err = test_client()
            .synthesize("テスト", voice)
            .await
            .expect_err("存在しないスタイル ID なのに成功した");
        assert!(matches!(err, Error::Status { .. }), "実際のエラー: {err}");
    }
}
