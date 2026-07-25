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

- 音声認識（STT）、多言語 TTS、音楽再生機能

---

## 3. 機能要件

| コマンド | 内容 |
| --- | --- |
| `/join` | 実行者が居る VC に接続。実行チャンネルを読み上げ対象に登録 |
| `/leave` | VC から切断、キュー破棄 |
| `/voice <speaker>` | 自分の話者を設定（オートコンプリートで一覧提示） |
| `/speed` `/pitch` `/intonation` | 音声パラメータ設定 |
| `/dict add <表記> <読み>` | ユーザー辞書登録（サーバー単位） |
| `/dict list` `/dict remove` | 辞書の確認・削除 |
| `/skip` | 現在再生中の読み上げをスキップ |
| `/config` | 現在のサーバー設定表示 |

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
| 読み上げ文字数上限 | 1 メッセージ 100 文字（超過分は「以下略」） |
| 障害時挙動 | ENGINE 停止時はエラーを 1 度だけ通知し、以降サイレント |
| リソース | VOICEVOX CPU 版で 2〜4 スレッド割当 |

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
4. **辞書適用**: サーバー辞書で置換、または ENGINE の `/user_dict_word` に登録して ENGINE 側で解決
5. **合成**
   - `POST /audio_query?text=...&speaker={id}` → AudioQuery(JSON)
   - JSON の `speedScale` / `pitchScale` / `intonationScale` をユーザー設定で上書き
   - `outputSamplingRate: 48000`, `outputStereo: true` に設定（**Discord が 48kHz ステレオのため、ここで合わせるとリサンプリングを省ける**）
   - `POST /synthesis?speaker={id}` （body に AudioQuery）→ wav バイト列
6. **キュー投入**: `SpeechTask { guild_id, wav: Vec<u8> }` を送信
7. **再生**: songbird の `Call::play_input()` に wav を渡す。再生完了を待って次へ

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

```sql
CREATE TABLE guild_settings (
    guild_id      INTEGER PRIMARY KEY,
    read_channel  INTEGER,          -- 読み上げ対象チャンネル
    max_length    INTEGER DEFAULT 100,
    read_bots     INTEGER DEFAULT 0,
    ignore_prefix TEXT DEFAULT ';'
);

CREATE TABLE user_voice (
    guild_id   INTEGER NOT NULL,
    user_id    INTEGER NOT NULL,
    speaker    INTEGER NOT NULL DEFAULT 3,   -- ずんだもん(ノーマル)
    speed      REAL    NOT NULL DEFAULT 1.0,
    pitch      REAL    NOT NULL DEFAULT 0.0,
    intonation REAL    NOT NULL DEFAULT 1.0,
    PRIMARY KEY (guild_id, user_id)
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
| RAM | 3 GB | ENGINE 実測 1.5〜2 GB を見込む |
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

| フェーズ | 内容 | 完了条件 |
| --- | --- | --- |
| 0. 検証 | ENGINE を Docker で起動し、curl で wav を取得 | ローカルで音が鳴る |
| 1. 骨組み | poise で `/join` `/leave`、無音接続 | VC に入退室できる |
| 2. 読み上げ | 固定話者でメッセージ読み上げ、キュー実装 | 連投しても順に読む |
| 3. 個人設定 | SQLite 永続化、`/voice` 等 | 再起動後も設定が残る |
| 4. 実用化 | テキスト正規化、辞書、自動退出、エラーハンドリング | 日常利用に耐える |
| 5. 運用 | Docker 化、Proxmox LXC へデプロイ、スナップショット設定 | 常時稼働・ロールバック可能 |

---

## 12. リスク・検討事項

| 項目 | 内容 | 対応 |
| --- | --- | --- |
| **ライセンス表記** | VOICEVOX 利用時はクレジット表記が必須。音声ライブラリごとに規約があり、キャラクター名を含めた表記（例:「VOICEVOX:ずんだもん」）が求められる | README・`/about` コマンド・Bot プロフィールに明記。使用する話者の利用規約を事前確認 |
| CPU 負荷 | CPU 合成はコア数依存。連投時に詰まる可能性 | スレッド数調整、キュー長上限、長文トリム |
| 初回発話の遅延 | モデルロードで数秒かかる | `initialize_speaker` によるウォームアップ |
| Discord Gateway 制限 | Message Content Intent が必須（Developer Portal で有効化） | 事前に設定 |
| 音質・サンプリングレート | 24kHz 出力だとリサンプリングが必要 | AudioQuery で 48kHz ステレオ指定 |
| 大量ギルド展開 | 公開 Bot 化すると合成負荷が跳ねる | 当面はプライベート運用に限定 |

---

## 13. 未決事項（実装前に確認すること）

1. 対象は自分のサーバーのみか、公開 Bot にするか（公開するなら §12 の負荷・権限設計を厚くする）
2. 話者設定はギルド単位で持つ（現行スキーマ）か、ユーザー単位でギルド横断にするか
3. 読み上げ対象チャンネルを複数登録できるようにするか（現行スキーマは 1 ギルド 1 チャンネル）
4. 発言者名の読み上げ（「いわ、こんにちは」）が必要か。必要なら「N 秒以上間が空いたときだけ前置」が実用的
5. LXC で行くか VM で行くか（§10.1、実機で判断）

---

## 14. 参考

- VOICEVOX ENGINE（GitHub / Docker Hub `voicevox/voicevox_engine`）
- ENGINE の API ドキュメント: 起動後 `http://localhost:50021/docs`（Swagger UI）
- serenity / poise / songbird の各リポジトリ example
