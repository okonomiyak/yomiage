-- 音楽再生統計（/stats）。曲別（タイトル単位）とユーザー別（リクエスト数）を集計する。
-- 再生リストの一括登録は数えない。個々に /play・/up_play したものだけを対象にする。

CREATE TABLE IF NOT EXISTS music_stats (
    guild_id INTEGER NOT NULL,
    user_id  INTEGER NOT NULL,
    title    TEXT NOT NULL,
    plays    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (guild_id, user_id, title)
);
