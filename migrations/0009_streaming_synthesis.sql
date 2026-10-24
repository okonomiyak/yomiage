-- ストリーミング合成（ENGINE 0.26.0 の /streaming_synthesis）。長文の喋り出しが速くなる。
-- 既定は無効（ENGINE が 0.26.0 未満だと合成できなくなるため、明示的に有効にする）。
ALTER TABLE guild_settings ADD COLUMN streaming_enabled INTEGER NOT NULL DEFAULT 0;
