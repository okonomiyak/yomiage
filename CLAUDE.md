# CLAUDE.md

VOICEVOX を使った Discord 読み上げ Bot。Rust 製。詳細仕様は `docs/PLAN.md` を参照すること。

## プロジェクト概要

Discord のテキストチャンネルの発言を VOICEVOX ENGINE で音声合成し、ボイスチャンネルで読み上げる。ENGINE は別コンテナで動き、HTTP API 経由で疎結合。永続化は SQLite。デプロイ先は Proxmox 上の LXC（Docker Compose）。

## 技術スタック

serenity + poise（Discord / スラッシュコマンド）、songbird（音声送信）、reqwest（ENGINE 呼び出し）、tokio、sqlx + SQLite、tracing、anyhow / thiserror。

**バージョンは `Cargo.toml` で固定する。** これらのクレートは API 破壊が多いので、書く前に固定したバージョンの docs.rs と examples を読むこと。記憶で書かない。特に songbird の `Input` 生成まわりは頻繁に変わる。

## 作業の進め方

- `docs/PLAN.md` §11 のロードマップ順に実装する。フェーズを飛ばさない。
- **1 セッション = 1 フェーズ**。完了条件を満たしたらコミットして終了する。次のフェーズは新しいセッションで始める。
- 仕様に書かれていないことを勝手に足さない。判断が必要なら `docs/PLAN.md` §13（未決事項）を確認し、そこにも無ければ実装せずに質問する。
- 大きな設計判断をしたら `docs/PLAN.md` に反映する。コードとドキュメントを乖離させない。

## コーディング規約

- `cargo clippy -- -D warnings` と `cargo fmt --check` が通る状態を維持する。コミット前に必ず実行。
- **`unwrap()` / `expect()` は起動時の設定読み込み以外で使わない。** 合成失敗・ENGINE 停止・Discord API エラーで Bot 全体が落ちてはいけない。エラーはログに残して該当メッセージをスキップする。
- エラー型は層で分ける。ENGINE クライアント層は `thiserror` で独自型、アプリ層は `anyhow`。
- ログは `tracing`。`println!` を使わない。合成レイテンシなど後でメトリクス化する値は span / field に載せておく。
- ID は型で区別する。VOICEVOX の `speaker` パラメータに渡すのは**キャラ ID ではなくスタイル ID**（`/speakers` の `styles[].id`）。`StyleId(u32)` の newtype を作って混同を防ぐ。

## テスト方針

- **テキスト正規化ロジックは純粋関数に切り出し、テストを先に書く。** URL・カスタム絵文字・メンション・コードブロック・連続改行・文字数超過・正規化後に空文字になるケースを網羅する。ここは仕様がケースの集合なので TDD で進める。
- ENGINE クライアントは HTTP をモックせず、ローカルで ENGINE を立てて疎通テストを書く（`#[ignore]` を付けて通常の `cargo test` からは外す）。
- Discord 層は自動テストしない。手動確認でよい。

## よく踏む落とし穴

- **Message Content Intent** が必要。Discord Developer Portal で有効化されていないとメッセージ本文が空で届く。
- **初回発話の遅延**：起動時に `POST /initialize_speaker` でウォームアップしないと数秒待たされる。
- **サンプリングレート**：AudioQuery で `outputSamplingRate: 48000`, `outputStereo: true` を指定すると Discord 側でのリサンプリングを省ける。
- **連投時の詰まり**：ギルドごとに 1 本のキューを持ち、専用タスクが順次再生する。ENGINE への同時リクエストは semaphore で制限する。
- **クレジット表記**：VOICEVOX と使用キャラクターの利用規約上、表記が必須。README と `/about` に入れる。

## コマンド

```sh
cargo clippy -- -D warnings     # コミット前
cargo fmt
cargo test                      # 単体テスト
cargo test -- --ignored         # ENGINE 疎通テスト（要 ENGINE 起動）
docker compose up -d voicevox   # ENGINE のみ起動
docker compose up --build       # 全体
rg 'unwrap\(\)|expect\(' src/   # 定期チェック
```

## 触ってはいけないもの

- `.env`（実トークン）。`.env.example` のみ編集する。
- `data/`（本番 SQLite）。マイグレーションは `migrations/` に追加し、既存ファイルは書き換えない。
