-- 時報（毎正時 or 30分ごとに時刻を読み上げる）。既定は無効（既存サーバーが突然喋り出さないように）。
ALTER TABLE guild_settings ADD COLUMN time_signal_enabled INTEGER NOT NULL DEFAULT 0;
ALTER TABLE guild_settings ADD COLUMN time_signal_interval INTEGER NOT NULL DEFAULT 60;
ALTER TABLE guild_settings ADD COLUMN time_signal_style INTEGER NOT NULL DEFAULT 3;
