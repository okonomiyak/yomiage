-- 読み上げ統計（/stats）。文字数のみ、サーバー×ユーザー単位。
-- day は JST のエポック日数（UTC+9 固定オフセットを 86400 秒で割った商）。
-- 日付の見た目を持たず「日をまたいだか」の比較にだけ使う。

CREATE TABLE IF NOT EXISTS speech_stats (
    guild_id    INTEGER NOT NULL,
    user_id     INTEGER NOT NULL,
    day         INTEGER NOT NULL,
    today_chars INTEGER NOT NULL DEFAULT 0,
    total_chars INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (guild_id, user_id)
);
