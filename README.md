# yomiage-bot

Discord のテキストチャンネルの発言を [VOICEVOX](https://voicevox.hiroshiba.jp/) で音声合成し、ボイスチャンネルで読み上げる Bot。Rust 製。

VOICEVOX ENGINE は別コンテナで動かし、HTTP API 経由で疎結合にしている。設定は SQLite に永続化する。

## クレジット

**音声合成: [VOICEVOX](https://voicevox.hiroshiba.jp/)**

この Bot が生成した音声を公開・配布する場合は、**「VOICEVOX:（キャラクター名）」** の形式でクレジットを表記し、使用したキャラクターごとの利用規約を確認すること。規約はキャラクターによって異なる。

- [VOICEVOX 利用規約](https://voicevox.hiroshiba.jp/term/)
- Bot 内では `/about` で同じ内容を確認できる

## できること

- 指定したテキストチャンネルの発言を読み上げ（連投してもキューで順に処理）
- テキストチャンネルとボイスチャンネルの紐づけ（`/bind`）。VC とは別の「聞き専」チャンネルで入力する構成に対応
- ユーザーごとの話者・速さ・高さ・抑揚の設定（サーバーをまたいで共通）
- サーバー単位の読み上げ辞書
- yt-dlp を使った音楽再生（キュー対応。読み上げと同時に鳴らせる）
- exVOICE（収録済み音声素材）対応。対応する話者を選んでいるとき、発言が素材の文章と一致すれば合成せずにその音声を鳴らす
- VC の入退室アナウンスと配信開始（Go Live）のアナウンス、無人になったら自動退出
- URL・メンション・カスタム絵文字・コードブロックなどの正規化

## コマンド

| コマンド | 内容 |
| --- | --- |
| `/join` | ボイスチャンネルに接続し、読み上げを開始する |
| `/leave` | 切断してキューを破棄する |
| `/bind <テキストch> <ボイスch>` | テキストチャンネルをボイスチャンネルに紐づける（設定として保存される）|
| `/unbind <テキストch>` | 紐づけを解除する |
| `/voice <話者>` | 自分の話者を設定する（オートコンプリート対応）|
| `/speed` `/pitch` `/intonation` | 速さ 0.5〜2.0 / 高さ -0.15〜0.15 / 抑揚 0.0〜2.0 |
| `/skip` | 今読み上げている 1 件を飛ばす |
| `/maxlength <文字数>` | 1 メッセージで読み上げる最大文字数（1〜500、サーバー単位、既定 100）|
| `/dict add <表記> <読み>` | サーバー辞書に登録する |
| `/dict list` `/dict remove` | 辞書の確認・削除 |
| `/feature <機能> <有効/無効>` | 読み上げと音楽を個別に on/off する（サーバー単位）|
| `/config` | サーバーの設定と自分の声を表示する |
| `/about` | クレジット表記 |
| `/help` | 使い方を表示（実行者にだけ見える）|
| `/play <URL または検索語>` | yt-dlp で音楽をキューに積む（読み上げに被せて再生）|
| `/queue` | 再生中と待機中の曲を表示する |
| `/dashboard` | ボタンで操作するパネルを出す（一時停止・再開・次へ・停止）|
| `/next` | 今の曲を飛ばして次へ |
| `/stop` | 音楽を止めてキューを空にする（読み上げは止めない）|
| `/volume <0-100>` | 音楽の音量（サーバー単位、既定 30%）|

### `/join` の接続先の決まり方

1. 実行したチャンネルに `/bind` の紐づけがあれば、その VC に接続する
2. 無ければ、実行者が参加している VC に接続する

読み上げ対象は、接続先 VC に紐づいたテキストチャンネルがあればそれ。無ければ実行したチャンネル。

### 読み上げないもの

- Bot の発言（サーバー設定で切替可）
- `;` で始まる発言
- 正規化した結果が空になる発言（添付ファイルだけの場合は「ファイル」と読む）

## 動かす

### 必要なもの

- Docker / Docker Compose
- Discord Bot のトークン
- CPU 2 コア程度と 3 GB 程度の RAM（VOICEVOX ENGINE が大半を使う）

### 1. Discord 側の準備

[Developer Portal](https://discord.com/developers/applications) で Bot を作成し、**Bot タブで MESSAGE CONTENT INTENT を有効化する**。これを忘れるとメッセージ本文が空で届き、Gateway が close code 4014 で切断される。

必要な権限は「チャンネルを見る」「メッセージを送信」「接続」「発言」。

### 2. 設定

```sh
cp .env.example .env
```

`.env` に `DISCORD_TOKEN` を書く。`GUILD_ID` を設定すると、そのサーバーにだけコマンドを**即時登録**する（空にするとグローバル登録になり、反映に最大 1 時間かかる）。

### 3. 起動

```sh
docker compose up -d
```

ENGINE のヘルスチェックが通ってから Bot が起動する。ログは `docker logs -f yomiage-bot`。

ENGINE はホストにポートを公開しない（compose のネットワーク内でのみ到達可能）。手元から直接叩きたい場合は `compose.dev.yaml` を重ねる。

```sh
docker compose -f compose.yaml -f compose.dev.yaml up -d voicevox
sh scripts/verify-engine.sh          # /version → 合成 → wav ヘッダ検証まで通す
```

## 開発

```sh
cargo test                      # 単体テスト（ENGINE 不要）
cargo test -- --ignored         # ENGINE への疎通テスト（要 ENGINE 起動）
cargo clippy -- -D warnings
cargo fmt
```

テキスト正規化は `src/text.rs` の純粋関数に切り出してあり、テストで挙動を固定している。仕様変更はテストから書くこと。

### リモートへのデプロイ

`scripts/deploy.sh` は、コミット済みのツリーを Proxmox の LXC コンテナへ送ってビルド・再起動する。接続先は `.env` から読む。

```
PVE_HOST=root@<Proxmox ホスト>
CTID=110
```

```sh
sh scripts/deploy.sh
```

`git archive HEAD` を送るので、**コミットしていない変更は反映されない**。

### Proxmox での運用

`scripts/yomiage-ctl.sh` で状態確認・ログ・再起動・バックアップ・スナップショットをまとめて扱える。**Proxmox ホストでも LXC の中でも動く**（`pct` があればホスト、無ければコンテナ内と判断する）。スナップショット系だけはホスト専用。

```sh
scp scripts/yomiage-ctl.sh root@<PVE>:/usr/local/bin/yomiage
ssh root@<PVE> chmod +x /usr/local/bin/yomiage
```

CT 内のコピーは `deploy.sh` が毎回更新するので、手動で入れ直す必要はない。

```sh
yomiage status              # CT・コンテナ・ディスク・直近ログ
yomiage logs -f             # ログを追う
yomiage restart             # 再起動（VC から抜けてから落ちる）
yomiage rebuild             # イメージを作り直して差し替え
yomiage backup              # SQLite を .backup で安全に取得（14 世代保持）
yomiage snapshot [名前]     # バックアップを取ってから LXC スナップショット
yomiage rollback <名前>     # スナップショットに戻す（確認あり）
yomiage prune               # 未使用イメージと古いビルドキャッシュを掃除
```

SQLite は稼働中にファイルをコピーすると壊れうるので、`backup` は `sqlite3 .backup` を使う（使い捨てコンテナ経由なので CT に sqlite3 を入れる必要はない）。定期実行するなら Proxmox ホストの cron に置く。

```cron
0 4 * * * /usr/local/bin/yomiage backup >/dev/null 2>&1
```

### 構成

```
src/
  main.rs      エントリポイント、イベントハンドラ、起動処理
  voicevox.rs  ENGINE クライアント（独自エラー型）
  speech.rs    ギルドごとの読み上げキュー（合成タスク → 再生タスク）
  text.rs      テキスト正規化（純粋関数）
  db.rs        SQLite
  commands/    スラッシュコマンド
migrations/    SQLite のマイグレーション
docs/PLAN.md   設計と決定事項
```

読み上げはギルドごとに 2 本のタスクを持ち、間を容量 1 のチャンネルで繋いでいる。これにより再生中に次の合成が進む（先読み合成）。詳細は `docs/PLAN.md` を参照。

## 注意点

- **symphonia の `wav` / `pcm` feature が必須**。songbird は symphonia をコーデック無効で依存しているため、外すと合成は成功するのに再生だけ黙って失敗する。
- ビルドには `cmake` が要る（songbird の Opus が libopus をビルドするため）。Dockerfile では導入済み。
- 音楽再生には `yt-dlp` の実行ファイルが要る。Dockerfile がビルド時に最新版を取得する（YouTube 側の変更で壊れやすいので固定していない）。再生できなくなったら `yomiage rebuild` で作り直すと直ることが多い。
- 再起動すると VC から抜け、読み上げ対象の登録も破棄される。`/join` からやり直すこと。`/bind` の紐づけと声の設定は残る。

## ライセンス

[MIT](LICENSE)

音声合成部分は VOICEVOX に依存しており、**生成された音声の利用条件はこのライセンスではなく VOICEVOX と各キャラクターの利用規約に従う**。
