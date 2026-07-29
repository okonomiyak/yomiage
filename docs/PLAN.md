# VOICEVOX 読み上げ Discord Bot 計画書

## 0. Claude Code への指示

このドキュメントは実装指示書を兼ねる。以下を守ること。

**進め方**

- §11 のロードマップの順に実装し、各フェーズの完了条件を満たしたらコミットする。フェーズを飛ばさない。
- 仕様に書かれていないことを勝手に足さない。判断が必要な箇所は §14 の未決事項を確認し、そこにも無ければ質問する。
- 外部クレート（serenity / poise / songbird / sqlx）は **バージョン間で API 破壊が多い**。実装前に `Cargo.toml` で固定したバージョンの docs.rs と examples を必ず読み、記憶で書かない。特に songbird の `Input` 生成まわり。

**品質基準**

- テキスト正規化（§7-3）は純粋関数に切り出し、単体テストを厚く書く。URL・カスタム絵文字・メンション・コードブロック・文字数超過・空文字化の各ケースを網羅する。
- `unwrap()` / `expect()` は起動時の設定読み込み以外で使わない。合成失敗・ENGINE 停止で Bot 全体が落ちないこと。
- エラーは `anyhow`（アプリ層）/ `thiserror`（クライアント層）で分ける。
- `cargo clippy -- -D warnings` と `cargo fmt --check` が通る状態を維持する。

**最初にやること**

1. `.env.example`、`.gitignore`（`.env`, `data/`, `target/`）を用意
2. Docker Compose で ENGINE を立て、`curl` で wav が取れることを確認（§11 フェーズ 0）
3. その後 `cargo new` から着手

---

## 1. 目的・背景

Discord のボイスチャンネル参加者に対し、指定テキストチャンネルの発言を VOICEVOX で音声合成して読み上げる Bot を自作する。既存 Bot（読み上げ花子等）への依存をなくし、iwaserver 上で完結する自前運用とする。

**設計方針**

- 既存の Rust + Serenity/Poise 資産を活かす
- VOICEVOX ENGINE は独立コンテナ、Bot とは HTTP API で疎結合
- iwaserver の Docker Compose スタックに載せる

---

## 2. スコープ

### MVP（v0.1）

- VC 参加 / 退出（`/join`, `/leave`）
- 読み上げ対象チャンネルの紐付け
- テキスト → 音声合成 → 再生（キュー順次再生）
- ユーザーごとの話者（speaker）選択と永続化

### v0.2 以降

- 読み上げ辞書（サーバー単位のユーザー辞書）
- 速度 / 音高 / 抑揚のユーザー設定
- 入退室アナウンス（「〇〇が参加しました」）
- 自動退出（VC に人がいなくなったら leave）
- Web UI（設定画面、iwaservice.uk 連携）

### 非スコープ

- 音声認識（STT）、多言語 TTS

**変更 2026-07-26**: 「音楽再生機能」は非スコープから外し、実装した（利用者の要望による）。yt-dlp を使い、読み上げとは別トラックとして同じ VC に混ぜて流す。`/play` `/queue` `/next` `/stop` `/volume` と、ボタン操作の `/dashboard`。パネルのボタンはコレクタで待たず `InteractionCreate` を `custom_id` で振り分ける（再起動しても前に貼ったパネルが使えるようにするため）。キューは songbird の `builtin-queue` に任せる（自前で持つと再生完了の検知と状態同期を書くことになりずれる）。読み上げは `play_input` で直接鳴らすためキューには入らず、音楽と独立して動く。音量はギルド単位で永続化する。読み上げに被せる前提なので既定音量は 30% と低めにしてある。

---

## 3. 機能要件

| コマンド | 内容 |
| --- | --- |
| `/join` | 実行者が居る VC に接続。実行チャンネルを読み上げ対象に**追加**（§13-3 で複数登録可に決定）|
| `/leave` | VC から切断、キュー破棄、登録チャンネルを全解除 |
| `/voice <speaker>` | 自分の話者を設定（オートコンプリートで一覧提示）。**ユーザー単位でギルド横断**（§13-2）|
| `/speed` `/pitch` `/intonation` | 音声パラメータ設定 |
| `/dict add <表記> <読み>` | ユーザー辞書登録（サーバー単位） |
| `/dict list` `/dict remove` | 辞書の確認・削除 |
| `/skip` | 現在再生中の読み上げをスキップ |
| `/config` | 現在のサーバー設定表示 |
| `/maxlength <文字数>` | 読み上げ文字数の上限を変更（1〜500、サーバー単位）。実装で追加 |
| `/feature <機能> <有効/無効>` | 読み上げ・音楽を個別に on/off（サーバー単位）。無効化時は溜まっているキューも捨てる。実装で追加 |
| `/playlist add\|list\|remove` | よく聴く URL を名前で登録（サーバー共有）。`/play <名前>` で呼び出せる。実装で追加（§13-10） |
| `/stats` | 読み上げ文字数（サーバー合計・ユーザー別、当日と累計）。実装で追加（§13-11） |

### 読み上げ対象外（無視するメッセージ）

- Bot 自身 / 他 Bot の発言（設定で切替可）
- 接頭辞付きメッセージ（例: `;` 始まり）
- コードブロック（丸ごと「コード省略」に置換）
- 添付ファイルのみのメッセージ（「ファイル」等に置換）

---

## 4. 非機能要件

| 項目 | 目標 |
| --- | --- |
| 発話開始までの遅延 | 1 秒以内（短文・CPU 合成） |
| 同時稼働ギルド数 | 3〜5（家庭内利用想定） |
| 読み上げ文字数上限 | 1 メッセージ 100 文字（超過分は「以下略」）。**既定値であり `/maxlength` で 1〜500 に変更できる**（§4.1 のとおり長くすると発話開始が遅くなる）|
| 障害時挙動 | ENGINE 停止時はエラーを 1 度だけ通知し、以降サイレント |
| リソース | VOICEVOX CPU 版で 2〜4 スレッド割当 |

### 4.1 実測値（2026-07-26 / LXC 110・`VV_CPU_NUM_THREADS=2`・ずんだもんノーマル）

| テキスト長 | `/audio_query` | `/synthesis` | 生成音声長 | RTF |
| --- | --- | --- | --- | --- |
| 5 文字（こんにちは） | 0.004s | 0.53s | 1.06s | 0.50 |
| 20 文字 | 0.014s | 1.26s | 3.49s | 0.36 |
| 90 文字（上限付近） | 0.016s | 4.57s | 14.15s | 0.32 |

- `/audio_query` は無視できる。**コストはすべて `/synthesis`**。
- 「発話開始 1 秒以内」は**短文でのみ成立**。20 文字で 1.3 秒、上限付近では 4.6 秒かかる。
- RTF は 0.3〜0.5 で**合成のほうが再生より速い**ので、一度キューが流れ始めれば詰まらない。効くのは先頭の待ち時間だけ。→ §7 補足の先読み合成はフェーズ 2 で必須と判断する。
- `initialize_speaker` は 1 秒（モデルロード済みの状態）。起動時ウォームアップの効果は要再測定（コンテナ再作成直後に測ること）。

---

## 5. 技術スタック

| レイヤ | 採用 | 備考 |
| --- | --- | --- |
| 言語 | Rust (2021/2024 edition) | 既存資産に合わせる |
| Discord | serenity + poise | スラッシュコマンドは poise で定義 |
| 音声送信 | songbird | Opus エンコード込み |
| HTTP | reqwest (JSON, connection pool 再利用) | ENGINE への合成リクエスト |
| 非同期 | tokio | |
| 永続化 | SQLite (sqlx) | 設定・辞書。ファイル 1 個でバックアップ容易 |
| 設定 | 環境変数 + `config.toml` | トークンは env |
| ログ | tracing + tracing-subscriber | |
| 実行環境 | Docker（distroless or debian-slim） | iwaserver / Proxmox LXC |

### 5.1 固定バージョン（フェーズ 1 時点）

API 破壊が多い 3 つは `Cargo.toml` で `=` 完全固定。上げるときは docs.rs / examples を読み直すこと。

| クレート | 固定 | メモ |
| --- | --- | --- |
| poise | `=0.6.2` | serenity 0.12 系に対応 |
| serenity | `=0.12.5` | `default-features = false` + `voice` / `cache` など必要分のみ |
| songbird | `=0.6.0` | serenity `^0.12` 対応。symphonia は `^0.5` 系（0.6 ではない）|
| sqlx | `=0.8.6` | **0.9 系は Rust 1.94 以上を要求する**ため見送り（手元と Docker ビルダーは 1.92）。0.9 では sqlite の feature 名が `sqlite` → `sqlite-bundled` に変わっているので、上げるときは注意 |

- songbird 0.6 の Opus は `opus2` 経由。**`opus2` → `libopus_sys` のビルドスクリプトが `cmake` を呼ぶ**ので、ビルド環境に cmake が要る（Debian のビルダーイメージには入っていない。入れないと `failed to execute command: No such file or directory` で落ちる）。libopus は静的リンクされるので**実行イメージ側には何も要らない**。手元の Windows は cmake が入っていたので気付かず通っていた。
- docs.rs の songbird 0.6.0 はビルドに失敗しているため、API 確認は GitHub の `v0.6.0` タグの examples を見ること。
- **symphonia を自分で依存に足すこと（重要）**。songbird は symphonia を `default-features = false` で入れており、コーデックを 1 つも有効にしない。songbird が足すのは Opus / DCA / 生 PCM だけなので、**wav を再生するには `symphonia = { features = ["wav", "pcm"] }` が必須**。無いと合成は成功するのに再生だけ黙って失敗する。`speech::tests::engine_wav_is_playable_by_songbird` が回帰テスト（依存を外すと実際に落ちることを確認済み）。

---

## 6. アーキテクチャ

```
Discord Gateway
      │ (message create)
      ▼
┌─────────────────────────────┐        HTTP        ┌──────────────────┐
│  yomiage-bot (Rust)         │ ─────────────────▶ │ VOICEVOX ENGINE  │
│  ├ command handler (poise)  │  /audio_query      │  :50021          │
│  ├ text normalizer          │  /synthesis        │  (Docker)        │
│  ├ speech queue (per guild) │ ◀───────────────── │                  │
│  └ songbird driver ─────────┼──▶ Voice Gateway   └──────────────────┘
└──────────┬──────────────────┘
           │
      SQLite (settings / dict)
```

- Bot ↔ ENGINE は Docker network 内の内部通信（外部公開しない）
- ギルドごとに 1 本のキュー（`mpsc::Sender<SpeechTask>`）を持ち、専用タスクが順次再生

---

## 7. 処理フロー

1. **受信**: `EventHandler::message` でメッセージ取得
2. **フィルタ**: 対象チャンネルか / 無視条件に該当しないか判定
3. **正規化**（読み上げやすいテキストへ変換）
   - メンション `<@123>` → 表示名
   - チャンネル `<#123>` → 「#チャンネル名」
   - カスタム絵文字 `<:name:id>` → 「name」
   - URL → 「URL省略」
   - 連続する同一文字の圧縮（`wwwww` → `わらわら` 等）
   - 改行 → 「、」
   - 文字数トリム（上限超過時は末尾に「以下略」）

   **実装（フェーズ4 / `src/text.rs`）で確定した細部**

   - 適用順は コードブロック → インラインコード → URL → カスタム絵文字 → メンション → 辞書 → 笑い → 改行 → 連続文字圧縮 → 前後の空白と読点を除去 → 文字数トリム。コードブロックを最初に潰すのは、中の URL やメンションを拾わないため。
   - 解決できないメンションは 「誰か」/「ロール」/「#どこか」 にフォールバックする。
   - 連続する同一文字は **3 文字まで**に圧縮する。`w` / `ｗ` が 2 文字以上続いたら「わら」にする（1 文字だけの `w` は英単語を壊すので触らない）。
   - **前後の読点も落とす**。これをしないと改行だけのメッセージが「、」として読み上げられる。
   - 正規化後に空になったら読まない。ただし添付があれば「ファイル」と読む。
   - トリムは**バイトではなく文字数**で数える（マルチバイトで panic するため）。
4. **辞書適用**: サーバー辞書で置換、または ENGINE の `/user_dict_word` に登録して ENGINE 側で解決
5. **合成**
   - `POST /audio_query?text=...&speaker={id}` → AudioQuery(JSON)
   - JSON の `speedScale` / `pitchScale` / `intonationScale` をユーザー設定で上書き
   - `outputSamplingRate: 48000`, `outputStereo: true` に設定（**Discord が 48kHz ステレオのため、ここで合わせるとリサンプリングを省ける**）
   - `POST /synthesis?speaker={id}` （body に AudioQuery）→ wav バイト列
6. **キュー投入**: `SpeechTask { guild_id, wav: Vec<u8> }` を送信
7. **再生**: songbird の `Call::play_input()` に wav を渡す。再生完了を待って次へ

### 7.1 名前の読み上げ（§13-4 の決定）

**発言ごとの発言者名の前置はしない。** メッセージは本文だけを読む。

名前を読むのは **VC の入退室アナウンスのときだけ**（§2 の v0.2 機能、ロードマップ上はフェーズ 4）。

- 文言: 「{名前}が参加しました」/「{名前}が退出しました」/「{名前}さんが配信を開始しました」（Go Live の開始）
- 配信の開始は `VoiceState.self_stream` の立ち上がりで拾う。チャンネルの移動を伴わないので、移動判定より先に見る必要がある。
- 読み上げる名前は **サーバーニックネーム > 表示名(global name) > ユーザー名** の順に採用する。
- 名前も §7-3 の正規化を通す（絵文字だらけのニックネーム対策）。正規化後に空になったらアナウンスしない。
- アナウンスは Bot が接続している VC の入退室のみが対象。
- **アナウンスの話者は対象ユーザーの設定話者を使う**（決定 2026-07-26）。入ってきた本人の声で「〇〇が参加しました」と読む。未設定なら既定話者。

### 7.2 キューの実装（フェーズ 2 / `src/speech.rs`）

ギルドごとに 2 本のタスクを持ち、間を容量 1 のチャンネルで繋ぐ。

```text
enqueue ─▶ [text queue: 20] ─▶ 合成タスク ─▶ [audio queue: 1] ─▶ 再生タスク ─▶ songbird
```

- **先読み合成**は audio queue のバッファがそのまま実現する。再生タスクが 1 件再生している間に、合成タスクは次を合成してバッファに置ける。
- text queue が溢れたら**待たずに捨てる**。ここで待つと Discord のイベントハンドラごと止まる。
- ENGINE への同時リクエストは semaphore で 2 に制限（ENGINE の `VV_CPU_NUM_THREADS=2` に合わせる）。ギルドが増えても合成が殺到しない。
- 再生完了は `TrackEvent::End` / `TrackEvent::Error` の両方で待ち受ける。加えて**音声長 + 10 秒のタイムアウト**を保険に置く。イベントを取りこぼすとキューが永久に止まるため。
- `/skip` のために再生中の `TrackHandle` と通知をギルドごとに持つ。**停止と同時に待機側も起こす**こと。`stop()` で完了イベントが来る保証が読み取れないため、片方だけだとタイムアウトまでキューが止まる。`Notify` はトラックごとに作り直すので、余った通知が次の再生を巻き込むことはない。
- 合成失敗・再生失敗はそのメッセージだけ飛ばしてログに残す。タスクは死なせない。

### 7.3 音楽のシークバー（§13-7 の決定 / `src/commands/dashboard.rs`）

`/dashboard` のパネルに `▬▬▬◉▬▬▬  2:14 / 4:52` を出し、5 秒ごとにメッセージを編集して進める。

```text
/dashboard ─▶ パネル投稿 ─▶ Panels::start ─▶ [follow タスク: 5 秒ごと]
                                                  │
   ボタン押下 ─▶ 状態を変える ─▶ 即座に描き替え ──┘（押すたびに追い掛け直す）
```

- **バーの描画は純粋関数**（`music::progress_bar` / `music::format_time`）にしてテストを持つ。0 除算・長さ不明・位置が長さを超えるケースを網羅する。
- 目盛りは 15 個。4 分の曲で 1 目盛り 16 秒なので、5 秒間隔で十分滑らかに見える。
- 再生位置は `TrackHandle::get_info()` の `TrackState.position`。曲の長さは `/play` 時の `aux_metadata().duration` を自前の表に持つ（`music::TrackInfo`）。
- ボタンは `custom_id` に幅を持たせる（`music:seek:-60` など）。幅を増やすときは `SEEK_STEPS` に足すだけで済む。**大きい幅を用意しておくと、小さいほうを連打するより取り直しの回数が減る**（下記のとおり後方シークは 1 回ごとに取り直しになるため）。`custom_id` は外から作れるので、受け取り側で上限を見る。
- **シークの実体**は `TrackHandle::seek_async()`。ただし YouTube 音源は `HttpStream::is_seekable()` が `false` なので、**前方シークは効くが後方シークは songbird が `Compose`（yt-dlp）から取り直す**。数秒かかるため、ボタンのハンドラは先に `CreateInteractionResponse::Acknowledge`（DEFERRED_UPDATE_MESSAGE）を返してから動かし、終わってからメッセージを編集する。3 秒以内に ack しないと Discord が失敗表示にする。
- 終端ちょうどへ飛ばすとシーク直後に曲が終わるので、長さが分かっているときは 3 秒手前で止める（`SEEK_TAIL_MARGIN`）。
- **曲名は `[曲名](URL)` のリンクにする**（`music::track_link`）。URL は `AuxMetadata.source_url`（yt-dlp の `webpage_url`）から取るので、検索語で入れた曲でも実際に選ばれた動画へ飛べる。曲名は他人が付けたものなので `[` `*` `<` などを必ずエスケープする。エスケープを忘れるとリンクが壊れ、`<@123>` のような曲名はメンションとして解釈される。括弧を含む URL は**リンクにせず曲名だけ出す**（中途半端に組み立てて崩れるより良いため）。URL は `<...>` で囲む。囲まないと Discord がプレビューを展開し、`/queue` のように何曲も並ぶところが埋め込みだらけになる。poise の `CreateReply` には `SUPPRESS_EMBEDS` を立てる口が無いので、ここで抑えるしかない。

### 補足

- **先読み合成**: キューが空でない間に次のメッセージを並行合成しておくと体感遅延が減る（合成 CPU に余裕がある場合）
- **話者の事前初期化**: 起動時に `POST /initialize_speaker?speaker={id}` を叩き、初回発話の遅延（モデルロード）を回避
- **songbird への wav 投入**: songbird 0.4 系は symphonia を内蔵しており、バイト列をそのまま `Input` に変換可能。具体的な型変換は使用バージョンの API を要確認（`Cursor<Vec<u8>>` 経由 or `From<Vec<u8>>`）

---

## 8. VOICEVOX ENGINE API（利用予定エンドポイント）

| メソッド | パス | 用途 |
| --- | --- | --- |
| GET | `/speakers` | 話者一覧取得（`/voice` のオートコンプリート用にキャッシュ） |
| POST | `/audio_query` | テキスト → 合成用クエリ |
| POST | `/synthesis` | クエリ → wav |
| POST | `/initialize_speaker` | 話者のウォームアップ |
| POST | `/user_dict_word` | ユーザー辞書登録 |
| GET | `/user_dict` | 辞書一覧 |
| GET | `/version` | ヘルスチェック |

---

## 9. データモデル（SQLite）

§13 の 2 / 3 / 4 の決定（2026-07-26）を反映済み。

```sql
CREATE TABLE guild_settings (
    guild_id      INTEGER PRIMARY KEY,
    max_length    INTEGER DEFAULT 100,
    read_bots     INTEGER DEFAULT 0,
    ignore_prefix TEXT DEFAULT ';'
);

-- §13-3: 読み上げ対象チャンネルは 1 ギルドに複数登録できる
CREATE TABLE read_channels (
    guild_id   INTEGER NOT NULL,
    channel_id INTEGER NOT NULL,
    PRIMARY KEY (guild_id, channel_id)
);

-- §13-2: 話者設定はユーザー単位でギルド横断。guild_id は持たない
CREATE TABLE user_voice (
    user_id    INTEGER PRIMARY KEY,
    speaker    INTEGER NOT NULL DEFAULT 3,   -- ずんだもん(ノーマル)
    speed      REAL    NOT NULL DEFAULT 1.0,
    pitch      REAL    NOT NULL DEFAULT 0.0,
    intonation REAL    NOT NULL DEFAULT 1.0
);

CREATE TABLE dictionary (
    guild_id INTEGER NOT NULL,
    surface  TEXT    NOT NULL,
    reading  TEXT    NOT NULL,
    PRIMARY KEY (guild_id, surface)
);
```

---

## 10. デプロイ構成（Proxmox VE）

### 10.1 ホスト側の構成

iwaserver の Proxmox 移行に合わせ、**読み上げ Bot 専用の 1 コンテナ**を切って他サービスから隔離する。

| 項目 | 値 | 備考 |
| --- | --- | --- |
| 種別 | LXC（Debian 12, unprivileged, `nesting=1`, `keyctl=1`） | Docker を動かすため nesting/keyctl 必須 |
| vCPU | 3 | ENGINE の `VV_CPU_NUM_THREADS=2` + Bot 用に 1 |
| RAM | **8 GB**（当初 3 GB） | ENGINE が実測 2.2 GB。そこに Docker 内の Rust ビルドが乗ると 3 GB では足りず、メモリと SWAP を使い切って OOM 寸前になった（2026-07-26）。ビルドを CT 内で行う限りは余裕を持たせる |
| ディスク | 20 GB | ENGINE イメージがモデル込みで数 GB ある |
| ネットワーク | 既存ブリッジ + Tailscale（管理用） | Bot は外向き通信のみ、inbound 不要 |

**LXC で詰まったら VM に切り替えて良い**。unprivileged LXC 上の Docker は overlayfs / cgroup まわりで踏むことがある。30 分溶かすようなら判断を切り替える。ENGINE は GPU 不要なので VM でも性能差はほぼ出ない。

### 10.2 状態とバックアップ

- 永続データは SQLite 1 ファイルのみ。LXC 内の `/opt/yomiage/data/` に集約し、それ以外は使い捨てにする。
- Proxmox の **スナップショット + vzdump を日次**で取る。これでロールバック手段が確保できるので、アップデートは気軽にやってよい。
- SQLite は稼働中にファイルコピーすると壊れうる。バックアップは `sqlite3 bot.db ".backup"` を cron で回してから vzdump 対象に含める。

### 10.3 compose.yaml

```yaml
services:
  voicevox:
    image: voicevox/voicevox_engine:cpu-0.25.2   # latest は使わない（2026-07 時点の安定版）
    container_name: voicevox
    restart: unless-stopped
    environment:
      - VV_CPU_NUM_THREADS=2
    expose:
      - "50021"           # ホストには公開せず内部ネットワークのみ
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:50021/version"]
      interval: 30s
      timeout: 5s
      retries: 3
    networks: [yomiage]

  yomiage-bot:
    build: .
    restart: unless-stopped
    depends_on:
      voicevox:
        condition: service_healthy
    environment:
      - DISCORD_TOKEN=${DISCORD_TOKEN}
      - VOICEVOX_URL=http://voicevox:50021
      - DATABASE_URL=sqlite:/data/bot.db
    volumes:
      - ./data:/data
    networks: [yomiage]

networks:
  yomiage:
```

- ローカル検証時のみ ENGINE をホストに公開したいので、`ports` は `compose.dev.yaml` に分離した（`compose.override.yaml` にすると本番でも自動で読まれてしまうため、`-f compose.yaml -f compose.dev.yaml` と明示したときだけ効く）。本番の `compose.yaml` は上記のとおり `expose` のみ。
- Dockerfile はマルチステージ（`rust:bookworm` でビルド → `debian:bookworm-slim` で実行）。`cargo-chef` で依存キャッシュを効かせる。LXC 上でのビルドは遅いので、可能なら手元でビルドして registry 経由で配る運用も検討する。
- `restart: unless-stopped` + `depends_on: service_healthy` で ENGINE 起動待ち。ENGINE が落ちても Bot は生かし、読み上げをスキップしてログに残す。
- Prometheus に合成回数・合成レイテンシを export（既存 Grafana に載せる）。v1 では後回しでよいが、`/metrics` を足せる構造にしておく。

---

## 11. 開発ロードマップ

| フェーズ | 内容 | 完了条件 | 状況 |
| --- | --- | --- | --- |
| 0. 検証 | ENGINE を Docker で起動し、curl で wav を取得 | ローカルで音が鳴る | **完了 2026-07-26**（LXC 110 / `scripts/verify-engine.sh`）|
| 1. 骨組み | poise で `/join` `/leave`、無音接続 | VC に入退室できる | **完了 2026-07-26**（実サーバーで入退室確認）|
| 2. 読み上げ | 固定話者でメッセージ読み上げ、キュー実装 | 連投しても順に読む | **実装完了 2026-07-26**（ENGINE 疎通テストは実機で通過。連投の手動確認待ち）|
| 3. 個人設定 | SQLite 永続化、`/voice` 等 | 再起動後も設定が残る | **実装完了 2026-07-26**（デプロイ済み。手動確認待ち）|
| 4. 実用化 | テキスト正規化、辞書、自動退出、**入退室アナウンス（§7.1）**、エラーハンドリング | 日常利用に耐える | **実装完了 2026-07-26**（デプロイ済み。手動確認待ち）|
| 5. 運用 | Docker 化、Proxmox LXC へデプロイ、スナップショット設定 | 常時稼働・ロールバック可能 | **一部前倒し 2026-07-26**（Dockerfile / compose / deploy.sh は完了。スナップショットと vzdump は未着手）|

### 11.2 開発の回し方（Docker 化以降）

手元で `cargo run` する必要は無くなった。ENGINE と Bot が同じ compose ネットワークに載るので、SSH トンネルも `VOICEVOX_URL` の付け替えも不要。

```sh
# 手元（リポジトリのルート）で
git commit ...
sh scripts/deploy.sh          # HEAD を LXC へ送ってビルド・再起動
ssh "$PVE_HOST" "pct exec $CTID -- docker logs -f yomiage-bot"
```

- `deploy.sh` は `git archive HEAD` を送るので、**コミットしていない変更は反映されない**（警告は出る）。
- `.env`（実トークン）は deploy 対象外。LXC の `/opt/yomiage/.env` に置いたまま触らない。
- 依存を変えなければ cargo-chef のキャッシュが効き、再ビルドは src の分だけで済む。
- 手元から `cargo test -- --ignored` を回したいときだけ `compose.dev.yaml` の `engine-tunnel` を起動する。

### 11.1 フェーズ 2 に入る前の申し送り

- Discord 側は **MESSAGE CONTENT INTENT 有効化済み**（未有効だと close code 4014 で落ちる）。
- スラッシュコマンドの登録先は `GUILD_ID` で切り替える。値があるとそのサーバーのみ（即時反映・開発用）、空ならグローバル（全サーバー）。**切り替えたときは反対側の登録を消す**こと。残っていると一覧に同じコマンドが二重で出る。グローバルへ戻すときは Bot が居る全ギルドのスコープ登録を空で上書きする。
- songbird ドライバは `mix_mode: Stereo` / `crypto_mode: Aes256Gcm` で接続する。48kHz ステレオ wav をそのまま流す方針と一致。
- 依存の `davey`（DAVE 実装）が `[DAVE Binary] Received ...` を tracing ではなく直接標準出力に吐く。ログが汚れるのでフェーズ 5 のログ整理で対処を検討する。
- 合成レイテンシは §4.1 参照。先読み合成をフェーズ 2 で実装する。

---

## 12. リスク・検討事項

| 項目 | 内容 | 対応 |
| --- | --- | --- |
| **ライセンス表記** | VOICEVOX 利用時はクレジット表記が必須。音声ライブラリごとに規約があり、キャラクター名を含めた表記（例:「VOICEVOX:ずんだもん」）が求められる | README・`/about` コマンド・Bot プロフィールに明記。使用する話者の利用規約を事前確認 |
| CPU 負荷 | CPU 合成はコア数依存。連投時に詰まる可能性 | スレッド数調整、キュー長上限、長文トリム |
| 初回発話の遅延 | モデルロードで数秒かかる | `initialize_speaker` によるウォームアップ |
| Discord Gateway 制限 | Message Content Intent が必須（Developer Portal で有効化） | 事前に設定 |
| 音質・サンプリングレート | 24kHz 出力だとリサンプリングが必要 | AudioQuery で 48kHz ステレオ指定 |
| 大量ギルド展開 | 公開 Bot 化すると合成負荷が跳ねる | **プライベート運用で確定（§13-1）**。公開しないので対策不要 |

---

## 13. 未決事項

1. **決定 2026-07-26: プライベート運用**。自分のサーバーのみ。公開 Bot にはしないので、§12 の「大量ギルド展開」対策と権限設計の作り込みは不要。招待も手動で行う。
2. **決定 2026-07-26: ユーザー単位でギルド横断**。`user_voice` の主キーは `user_id` のみ（§9）。どのサーバーでも同じ声になる。
3. **決定 2026-07-26: 複数登録できるようにする**。`read_channels` テーブルへ分離（§9）。キューはギルドごとに 1 本のままで、複数チャンネルの発言が同じキューに合流する（§6）。
4. **決定 2026-07-26: 発言ごとの発言者名の前置は不要**。名前を読むのは VC の入退室アナウンスのときだけ（§7.1）。
6. **決定 2026-07-26（§7-4 の実装方法）: 辞書は ENGINE のユーザー辞書ではなくテキスト置換で当てる**。ENGINE の `/user_dict_word` は ENGINE 単位＝全ギルド共通になってしまい、「サーバー単位の辞書」（§2）と噛み合わないため。長い表記から順に適用して、短い表記が長い表記を壊さないようにする。
5. **決定 2026-07-26: LXC で行く**。LXC 110（Debian 13, unprivileged, nesting+keyctl）上の Docker で ENGINE が問題なく動くことをフェーズ 0 で確認済み。
7. **決定 2026-07-27: 音楽パネルのシークバーは「メッセージの定期編集」で作る**。Discord にスライダー部品は無いので、バーは文字（`▬▬◉▬▬  2:14 / 4:52`）で描き、`/dashboard` を貼ったギルドごとに 1 本のタスクが **5 秒ごとにそのメッセージを編集する**（`src/commands/dashboard.rs` の `Panels`）。
   - **描いた内容が前回と同じなら編集リクエストを出さない。** 一時停止中と無音のときに Discord を叩き続けないため。
   - **5 分間何も流れなければタスクを畳む。** 放置されたパネルを永久に追い掛けない。ボタンを押すと `Panels::start` が呼ばれて再開するので、畳まれた後でも復活する（Bot の再起動後も同じ経路で復活する）。
   - 曲の長さは `/play` の `aux_metadata()` から取って自前の表に持つ。バーを描くたびに yt-dlp を叩き直さないため。取れない曲（ライブ配信など）はバーを出さず経過時間だけ出す。
   - シークは ⏪ ⏩ のボタンのみ（±10 秒／±60 秒）。位置を直接指定する `/seek` は作らない。詳細は §7.3。
8. **決定 2026-07-27: ニコニコ動画は yt-dlp の標準出力を直接流す**（`src/nicovideo.rs`）。songbird の `YoutubeDl` は URL とヘッダだけ取り出して**取得を songbird 自身が行う**が、ニコニコの配信（domand）は抽出時に yt-dlp が確立する `domand_bid` Cookie を要求し、これが `http_headers` に含まれないため 403 になる。
   - `Compose` として実装し、**再生が始まる直前まで yt-dlp を起動しない**。キュー後方の曲が先にダウンロードを始めてパイプを詰まらせないため。
   - 音声は HLS / fMP4 + AAC が選ばれる。symphonia の `isomp4` と `aac` に依存する（どちらかを外すと取得は成功して音だけ出なくなる）。回帰テストは `nicovideo::tests::nicovideo_stream_is_playable_by_songbird`（`#[ignore]`）。
   - **URL のみ対応。検索語は従来どおり YouTube から探す**。yt-dlp には `nicosearch:` もあるが、どちらを引くかを利用者が選べないと混乱するため入れていない。
   - **会員限定・年齢制限・センシティブ指定の動画は Cookie ファイルで対応する**（決定 2026-07-27）。`NICO_COOKIES`（既定 `/data/nico_cookies.txt`）にログイン済みの Netscape 形式 Cookie を置くと、メタデータ取得と再生の両方に `--cookies` で渡る。未設定なら認証なしで動き、公開動画だけ再生できる。パスワードをサーバーに置かずに済むので、`--username/--password` や `--netrc` ではなくこちらを選んだ。
   - **原本は yt-dlp に渡さず、毎回コピーを渡す**（`cookie_copy()`）。yt-dlp は終了時に必ず Cookie を書き戻すが、その書き戻しは `expiry=0` のセッション Cookie を捨てる。原本を直接渡すと使うたびに削れていき、実機ではセッションごと無効化されて公開動画すら再生できなくなった。読み取り専用で渡すのも不可で、書き戻しに失敗して終了コードが 1 になる。コピーなら書き戻しを捨てられ、同時実行でも壊し合わない。
   - メタデータを引けない動画は**積む前に弾く**。メタデータも取得も同じ yt-dlp なので、ここで失敗するものは再生もできない。積んでしまうと「再生します」と出したきり無音になり、理由が伝わらない。
9. **決定 2026-07-29: 時報を追加する**（`src/timesignal.rs`）。Bot が**接続中の VC 全て**で、ギルドごとに設定した頻度（毎正時／30分おき）で「ただいま14時です」のように読み上げる。
   - on/off は `/feature` に「時報」を追加して切り替える（既存の読み上げ・音楽と同じ枠組み）。**既定は無効**。既存サーバーがデプロイ直後に突然喋り出さないようにするため。
   - 頻度と話者はギルド単位（`/timesignal interval` / `/timesignal voice`）。発言者がいないイベントなので、ユーザー単位の声（§13-2）とは別に持つ。
   - 実装は 1 分ごとに起きる単一のバックグラウンドタスクで、`songbird` が今接続しているギルドだけを毎回列挙してチェックする（`/join` 済みのギルド一覧を別途持たない）。タイムゾーンは JST 固定（UTC+9 の固定オフセット。夏時間が無いので足りる）。
10. **決定 2026-07-29: 音楽のお気に入り（`/playlist`）を追加する**。よく聴く URL を名前で登録し、`/play <名前>` からも呼べるようにする。
    - **サーバー共有**（`/dict` や音楽キューと同じくギルド単位）。個人用（ユーザー単位でギルド横断）にはしない。誰か 1 人が登録すればそのサーバーの全員が使える方が、家庭内利用の実態に合うため。
    - `/play` の引数は今まで通り URL/検索語のフリーテキストのまま。**登録名と完全一致したときだけ**その URL に差し替え、一致しなければ従来通り yt-dlp にそのまま渡す。`/play` 専用の名前引数は作らない。
    - オートコンプリートは `/playlist remove` と `/play` の両方に付ける（`autocomplete_playlist_name`）。ただし `/play` は自由入力を許すので、候補はあくまで補助。
11. **決定 2026-07-29: 読み上げ統計（`/stats`）を追加する**。文字数のみを数える（話者別ランキングは持たない）。
    - **サーバー×ユーザー単位**で集計する（`speech_stats` テーブル、`guild_id, user_id` が主キー）。`/stats` はサーバー合計とユーザー別内訳の両方を出す。
    - 持つのは「当日の文字数」と「累計文字数」の 2 値のみ（§13-9 の時報と同じく JST 固定オフセット）。日別の履歴は保存しない。日を跨いだかは行ごとに持つ `day`（JST のエポック日数）で判定し、一致しなければ当日分を 0 として扱う（全ギルド分をリセットするバッチは持たない）。
    - 数えるのは `text::normalize` を通した後、実際に読み上げに積んだ文字数（`handle_message` から `Db::record_speech_chars` を呼ぶ）。
12. **決定 2026-07-29: `/play` に YouTube 再生リスト・ニコニコのマイリスト/シリーズの取り込みを追加する**（`src/music.rs` の `enqueue_playlist` / `is_playlist_url`）。専用コマンドは作らず、`/play` に渡した URL が再生リストと判定されたときだけ自動でまとめて積む。
    - **対応するのは YouTube と ニコニコ（マイリスト・シリーズ）の両方**。どちらも `yt-dlp --flat-playlist -j` で列挙できるので、既存の `/play` の対象プラットフォームとも噛み合う。
    - **プレイリスト専用ページの URL だけを対象にする**（YouTube は `/playlist?list=...`、ニコニコは `/mylist/...` `/series/...`）。`watch?v=...&list=...` のように「リスト内の 1 曲を再生中」の URL までは拾わない。1 曲だけのつもりで貼った URL がリスト全体の取り込みになってしまうと事故になるため。
    - **1 回で積む曲数は 50 曲まで**。超えた分は捨てて件数だけ伝える。キューが一気に埋まって長時間占有するのを防ぐ。
    - **曲ごとのタイトル・長さは `--flat-playlist` の一覧情報のみを使う**。既存の単曲 `/play`（`describe()`）のように曲ごとに追加で yt-dlp を叩き直すと、大きなリストで `/play` の応答が大きく遅れるため。正確な長さは実際に再生されるときに yt-dlp が別途取得するので、`/queue` 上は取れなかった曲が「（長さ不明）」のままになることがある。
    - 一覧に出てくる `url` フィールドは動画 ID だけのことがある（yt-dlp のバージョン依存）。`id` から `https://www.youtube.com/watch?v=<id>` または `https://www.nicovideo.jp/watch/<id>` を組み立て直すフォールバックを持つ（`FlatEntry::resolve_url`）。
    - **ニコニコだけは曲ごとに `aux_metadata()` で再生可否を事前確認する**（決定 2026-07-29、実機検証後に追記）。実際にマイリストを取り込んだところ、会員限定・センシティブ指定の動画が無音のまま飛ばされ、理由がまったく伝わらなかった（`ERROR: [niconico] ...: Invalid session, re-login required` がログに出るだけで Discord 側には何も表示されない）。ニコニコは単曲 `/play` と同じ理由（メタデータ取得と実際の取得が同じ yt-dlp 呼び出しなので、ここで弾けるものは再生もできない）で事前確認を入れる。再生できない曲は件数だけ利用者に伝える（`PlaylistQueued::unplayable`）。**YouTube は今まで通り事前確認しない**（`/play` の応答がリストの曲数分遅れるのを避けるため。YouTube はニコニコほど「メタデータは引けるが実際には再生できない」ケースが多くない）。

### 13-A 決定 3 から派生した未決事項（項目 13-1 とは別物）

複数チャンネル登録にしたことで、**登録解除の手段**が仕様に無い状態になった。現状の想定は「`/join` = 実行チャンネルを追加、`/leave` = 切断して**全登録を破棄**」だが、切断せずに 1 チャンネルだけ外す操作ができない。フェーズ 3（`/config` 実装）までに決めること。

---

## 14. 参考

- VOICEVOX ENGINE（GitHub / Docker Hub `voicevox/voicevox_engine`）
- ENGINE の API ドキュメント: 起動後 `http://localhost:50021/docs`（Swagger UI）
- serenity / poise / songbird の各リポジトリ example
