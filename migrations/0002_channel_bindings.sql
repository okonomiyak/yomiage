-- テキストチャンネルとボイスチャンネルの紐づけ。
--
-- read_channels（今どこを読み上げ中か）は起動時に消すセッション状態だが、
-- こちらは設定なので消さない。両者を同じテーブルにすると再起動で紐づけが消える。

CREATE TABLE IF NOT EXISTS channel_bindings (
    guild_id         INTEGER NOT NULL,
    text_channel_id  INTEGER NOT NULL,
    voice_channel_id INTEGER NOT NULL,
    PRIMARY KEY (guild_id, text_channel_id)
);
