# 引き継ぎ書

このプロジェクトを引き継ぐ人（または久しぶりに触る自分）向けのメモ。
仕様と決定事項は [PLAN.md](PLAN.md)、使い方は [README](../README.md) にある。
ここには**現在の状態**と**実際に踏んだ落とし穴**を書く。

最終更新: 2026-07-27

---

## 1. 何を作ったか

Discord のテキストチャンネルの発言を VOICEVOX で読み上げる Bot（Rust）。
自宅の Proxmox 上の LXC で Docker Compose で常時稼働している。

途中から要望で機能が増え、当初の仕様（PLAN §2）から次の点が変わっている。

| 項目 | 当初 | 現在 |
| --- | --- | --- |
| 音楽再生 | **非スコープ** | 実装済み（yt-dlp、キューとパネル付き）|
| 話者設定の単位 | ギルド単位 | **ユーザー単位でギルド横断**（§13-2）|
| 読み上げチャンネル | 1 ギルド 1 つ | **複数**＋ VC との紐づけ（§13-3）|
| 発言者名の読み上げ | 検討中 | **しない**。名前を読むのは入退室と配信のアナウンスだけ（§13-4）|
| 公開範囲 | 未決 | **プライベート運用**（§13-1）|

---

## 2. 今の状態

ロードマップ（PLAN §11）のフェーズ 0〜5 はすべて実装済み。フェーズ 5 のうち
**Proxmox のスナップショット定期取得と vzdump 連携だけ未着手**。

コマンドは 21 個。グローバル登録（全サーバーで使える）。

| 分類 | コマンド |
| --- | --- |
| 読み上げ | `/join` `/leave` `/bind` `/unbind` `/skip` |
| 自分の声 | `/voice` `/speed` `/pitch` `/intonation` |
| 音楽 | `/play` `/queue` `/next` `/stop` `/volume` `/dashboard` |
| サーバー設定 | `/feature` `/maxlength` `/dict` `/config` |
| その他 | `/help` `/about` |

読み上げ以外の主な挙動:

- `/dashboard` のパネルにシークバーが出る。5 秒ごとに本文を編集して進み、
  ⏪ ⏩ で 10 秒ずつ動かせる。5 分間何も流れないと編集をやめ、ボタンを押すと再開する
- VC が無人になると自動退出し、テキストチャンネルに通知する
- 入退室と配信（Go Live）の開始・終了をアナウンスする（本人の設定話者で読む）
- 再起動時は VC から抜けてから終了し、起動時に読み上げ登録を全消去する
  （`/bind` の紐づけと声の設定は残る）
- ENGINE が落ちたときは**発生時に 1 度だけ**通知し、復旧までサイレント

### 未確認・未完了

- **exVOICE の素材をサーバーへ置いたかどうか**（下記 6 参照）。置いていなければ
  機能が無効なだけで、Bot は合成のみで正常に動く。起動ログの
  `exvoice library loaded count=N` で判断できる
- Bot プロフィールへのクレジット記載（Developer Portal での手作業）
- スナップショットの定期取得（`yomiage snapshot` は手動で使える）

---

## 3. 開発と反映

```sh
cargo test                  # 単体テスト 44 本（ENGINE 不要）
cargo test -- --ignored     # ENGINE 疎通テスト 5 本（要 ENGINE）
cargo clippy -- -D warnings
cargo fmt
sh scripts/deploy.sh        # コミット済みの HEAD をサーバーへ送ってビルド・再起動
```

`deploy.sh` の接続先は **`.env`（git 管理外）から読む**。公開リポジトリに自宅の
アドレスを置かないため。次の 2 行が必要:

```
PVE_HOST=root@<Proxmox ホスト>
CTID=110
```

運用は `scripts/yomiage-ctl.sh`（`yomiage` としてインストール済み）。
Proxmox ホストでも LXC の中でも動く。

```sh
yomiage status | logs -f | restart | rebuild | backup | prune | snapshot | rollback
```

---

## 4. 構成

```
src/
  main.rs      起動、イベントハンドラ（メッセージ／音声状態／ボタン）、終了処理
  voicevox.rs  ENGINE クライアント。独自エラー型（thiserror）
  speech.rs    ギルドごとの読み上げキュー
  music.rs     音楽再生。キューは songbird の builtin-queue に任せる
               進捗バーの描画は純粋関数（progress_bar / format_time、テスト 9 本）
  exvoice.rs   収録済み音声素材の対応表
  text.rs      テキスト正規化（純粋関数、テスト 20 本）
  db.rs        SQLite（sqlx）
  commands/    スラッシュコマンド
migrations/    0001 初期 / 0002 紐づけ / 0003 音量 / 0004 機能スイッチ
```

読み上げキューはギルドごとに 2 本のタスクで、間を容量 1 のチャンネルで繋いでいる。
このバッファがそのまま**先読み合成**になる（再生中に次を合成できる）。詳細は PLAN §7.2。

---

## 5. 踏んだ落とし穴（重要）

ここが一番の引き継ぎ価値。同じ罠を二度踏まないこと。

### ビルド・依存

- **symphonia のコーデックは自分で有効にする。** songbird は symphonia を
  `default-features = false` で入れており、コーデックを 1 つも有効にしない。
  wav / mkv / isomp4 / aac を落とすと、**合成や取得は成功するのに再生だけ黙って失敗する**。
  回帰テスト `speech::tests::engine_wav_is_playable_by_songbird` あり
- **ビルドに cmake が要る。** songbird の Opus（`opus2` → `libopus_sys`）が呼ぶ。
  Windows では偶然入っていて気付かなかった。実行イメージ側には何も要らない（静的リンク）
- **sqlx は 0.8 系で止めている。** 0.9 は Rust 1.94 以上を要求する。0.9 では
  sqlite の feature 名が `sqlite` → `sqlite-bundled` に変わっている
- **songbird 0.6 の `TrackHandle::data()` は型が違うと panic する。** 曲名の保持には
  使わず、自前の UUID→曲情報（タイトルと長さ）の表にしてある
- **YouTube 音源は後方シークが遅い。** `src/input/sources/http.rs` の
  `HttpStream::is_seekable()` が `false` を返すので、`TrackHandle::seek_async()` の
  前方シークは効くが、**巻き戻しは songbird が `Compose`（yt-dlp）から丸ごと
  取り直す**。⏪ を連打するとそのたびに再取得が走る。パネルのボタンは先に
  `Acknowledge` を返してから動かすこと（3 秒以内に ack しないと Discord が失敗表示にする）
- **docs.rs の songbird 0.6.0 はビルドに失敗していて API ドキュメントが無い。**
  GitHub の `v0.6.0` タグの examples とソースを読むこと

### Discord

- **MESSAGE CONTENT INTENT** を有効にしないと close code 4014 で切断される
- **コマンド登録先の切り替えでは反対側を消す。** ギルド限定とグローバルの両方に
  残ると一覧に二重で出る。`GUILD_ID` 環境変数で切り替え、消す処理も実装済み
- **`/play` は defer が要る。** yt-dlp の起動で 3 秒を超え、応答なし扱いになる
- **ボタンで時間のかかる処理をするときは `Acknowledge`（DEFERRED_UPDATE_MESSAGE）。**
  `Defer` だと「考え中」表示が出てしまう。ack した後は `UpdateMessage` が使えないので、
  メッセージを `ChannelId::edit_message` で直接編集する
- **interaction のトークンは 15 分で切れる。** シークバーのように後から編集し続ける
  ものは、貼ったときに `MessageId` を取っておいて直接編集する
- **オートコンプリートと日本語入力は相性が悪い。** 変換確定前の文字列が飛んでくるので
  漢字の話者名に一致しない。スタイル ID でも引けるようにして回避している

### 環境

- **`pct exec` 経由のコマンドに日本語を直接埋めると壊れる。** 多段クォートで
  文字化けする。スクリプトファイルを送って実行するか、ASCII だけで書く
- **Windows は 50000-50059 を予約済みポートにしていることがある。** `ssh -L` の
  ローカル bind が Permission denied で弾かれる。範囲外を使う
- **Tailscale SSH は定期的に再認証を要求する。** これが起きると ssh が無言でハングし、
  「サーバーが重い」ように見える。**応答が無いときはまず認証エラーを疑う**こと
  （一度これで誤診して時間を溶かした）
- **CT のメモリは 8GB 必要。** ENGINE が 2.2GB 使うところに Docker 内の Rust ビルドが
  乗ると 3GB では足りず、SWAP まで使い切って OOM 寸前になる

### git

- **`git add -A` は危険。** 作業ディレクトリに置かれた 166MB の音声素材 zip を
  巻き込んでコミットし、GitHub の 100MB 制限で push が弾かれた。
  素材類は先に `.gitignore` へ入れること（`/exVOICE/` は追加済み）
- **公開リポジトリなのでサーバーのアドレスを書かない。** 一度履歴に入れてしまい、
  `filter-branch` で全コミットから消してから push した

---

## 6. exVOICE（収録済み音声素材）

冥鳴ひまり（スタイル ID 14）を選んでいるユーザーの発言が素材の文章と一致したら、
合成せずにその wav を鳴らす。

- 対応表は**同梱 CSV ではなく実ファイルの走査**で作る。CSV には実在しない行が
  2 件あり、鳴らせないキーを持つと再生時に初めて失敗するため
- キーはファイル名から先頭の `番号_` を落としたもの（＝ CSV の「ファイル名」列）
- 素材は 559MB あり**リポジトリには入れない**。サーバーの `/opt/yomiage/exVOICE` に
  直接置き、compose が `/exvoice` にマウントする
- ディレクトリが無ければ空のまま起動し、合成のみで動く（エラーにしない）

別のキャラの素材を足すなら `exvoice.rs` の `STYLE` を複数対応にする必要がある。
今は 1 キャラ決め打ち。

---

## 7. 次にやるとよいこと

1. **長文の発話開始を速くする**（効果が大きい）
   実測で 90 文字の合成に 4.6 秒かかる（PLAN §4.1）。`/maxlength` を 500 まで
   上げられるようにしたので、上限付近だと 25 秒黙ってから喋り出す。
   ENGINE 側のストリーミング合成は
   [voicevox_engine#1492](https://github.com/VOICEVOX/voicevox_engine/issues/1492)
   で検討中だが**未実装**。こちらで `。！？` ごとに分割して順に投げれば、
   最初の 1 文だけ合成すれば喋り始められる。既存のキューがそのまま使える。
   注意点は、1 メッセージが複数タスクになるのでキュー上限（20）の数え方を
   見直す必要があること
2. スナップショットの定期取得（cron から `yomiage snapshot`）
3. Prometheus への合成回数・レイテンシの export（PLAN §10.3。`/metrics` を
   足せる構造にはしてある）
4. 非 root で動かす（今は root。`/data` の所有権を合わせる必要がある）

---

## 8. 触ってはいけないもの

- `.env`（実トークンと接続先）。`.env.example` のみ編集する
- `data/`（本番 SQLite）。マイグレーションは `migrations/` に**追加**し、
  既存ファイルは書き換えない
- `exVOICE/`（配布物）。リポジトリに入れない
