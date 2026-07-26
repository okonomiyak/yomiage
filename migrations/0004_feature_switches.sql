-- 読み上げと音楽をギルドごとに個別で止められるようにする。既定はどちらも有効。
ALTER TABLE guild_settings ADD COLUMN tts_enabled INTEGER NOT NULL DEFAULT 1;
ALTER TABLE guild_settings ADD COLUMN music_enabled INTEGER NOT NULL DEFAULT 1;
