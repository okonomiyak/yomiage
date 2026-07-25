-- PLAN §9 のデータモデル。§13 の決定を反映済み。
-- 既存のマイグレーションは書き換えない。変更は新しいファイルを足すこと。

CREATE TABLE IF NOT EXISTS guild_settings (
    guild_id      INTEGER PRIMARY KEY,
    max_length    INTEGER NOT NULL DEFAULT 100,
    read_bots     INTEGER NOT NULL DEFAULT 0,
    ignore_prefix TEXT    NOT NULL DEFAULT ';'
);

-- §13-3: 読み上げ対象チャンネルは 1 ギルドに複数登録できる
CREATE TABLE IF NOT EXISTS read_channels (
    guild_id   INTEGER NOT NULL,
    channel_id INTEGER NOT NULL,
    PRIMARY KEY (guild_id, channel_id)
);

-- §13-2: 話者設定はユーザー単位でギルド横断。guild_id は持たない
CREATE TABLE IF NOT EXISTS user_voice (
    user_id    INTEGER PRIMARY KEY,
    speaker    INTEGER NOT NULL DEFAULT 3,   -- ずんだもん(ノーマル) のスタイル ID
    speed      REAL    NOT NULL DEFAULT 1.0,
    pitch      REAL    NOT NULL DEFAULT 0.0,
    intonation REAL    NOT NULL DEFAULT 1.0
);

-- 辞書はフェーズ 4 で使う。テーブルだけ先に作っておく。
CREATE TABLE IF NOT EXISTS dictionary (
    guild_id INTEGER NOT NULL,
    surface  TEXT    NOT NULL,
    reading  TEXT    NOT NULL,
    PRIMARY KEY (guild_id, surface)
);
