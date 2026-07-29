-- お気に入り音楽（サーバー共有）。/playlist で登録した名前を /play からも呼べる。

CREATE TABLE IF NOT EXISTS playlist (
    guild_id INTEGER NOT NULL,
    name     TEXT    NOT NULL,
    url      TEXT    NOT NULL,
    PRIMARY KEY (guild_id, name)
);
