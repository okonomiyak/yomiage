-- 音楽の音量（0.0〜1.0）。読み上げに被せて流すので、既定は小さめにしておく。
ALTER TABLE guild_settings ADD COLUMN music_volume REAL NOT NULL DEFAULT 0.3;
