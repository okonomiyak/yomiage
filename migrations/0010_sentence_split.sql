-- 長文を文ごとに分けて ENGINE に投げる（PLAN §13-18）。既定は有効（喋り出しが速く、ENGINE のメモリも膨らみにくい）。
ALTER TABLE guild_settings ADD COLUMN sentence_split_enabled INTEGER NOT NULL DEFAULT 1;
