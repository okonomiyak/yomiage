#!/bin/sh
# 本番(LXC 110, tts-bot)のDBを予備機(別Proxmoxホストの LXC 111)へ定期反映する。
#
#   sh scripts/sync-standby-db.sh
#
# Proxmoxホスト(本番側 = pve)で実行する想定（pct が要る）。cron に登録して使う。
# ノード間は同一クラスタなので root の鍵ログインがそのまま通る。
#
# STANDBY_HOST（予備機の Proxmox ホスト）は必須。公開リポジトリにアドレスを置かないため、
# cron 側で渡す:
#   17 * * * * STANDBY_HOST=root@<予備機> /usr/local/bin/yomiage-sync-standby-db.sh
# 環境変数で上書きできる: SRC_CTID / DST_CTID / DEST
#
# 稼働中の SQLite ファイルを直接コピーすると壊れうるので、本番側は .backup で
# 安全なスナップショットを取ってから転送する（PLAN §10.2 と同じ考え方）。
# 予備機側は待機系（/join していない）なので、書き換え中だけ tts-bot を止めて
# 差し替え後に起動し直す。voicevox は止めない。

set -eu

# cron の PATH には /usr/sbin が無く、pct が見つからない。
PATH="/usr/sbin:/usr/bin:/sbin:/bin:$PATH"

SRC_CTID="${SRC_CTID:-110}"
DST_CTID="${DST_CTID:-111}"
STANDBY_HOST="${STANDBY_HOST:?STANDBY_HOST に予備機の Proxmox ホストを指定してください（例: root@10.0.0.2）}"
DEST="${DEST:-/opt/yomiage}"
STAMP=$$
LOCAL_TMP="/tmp/yomiage-standby-sync-$STAMP.db"
REMOTE_TMP="/tmp/yomiage-standby-sync-$STAMP.db"

cleanup() {
    rm -f "$LOCAL_TMP"
    pct exec "$SRC_CTID" -- rm -f /var/backups/yomiage/standby-sync.db 2>/dev/null || true
}
trap cleanup EXIT

echo "== 本番(CT $SRC_CTID)でスナップショット取得"
pct exec "$SRC_CTID" -- mkdir -p /var/backups/yomiage
pct exec "$SRC_CTID" -- docker run --rm \
    -v "$DEST/data:/data:ro" -v /var/backups/yomiage:/backup alpine:3 \
    sh -c "apk add --no-cache sqlite >/dev/null 2>&1 && sqlite3 /data/bot.db \".backup '/backup/standby-sync.db'\""
pct pull "$SRC_CTID" /var/backups/yomiage/standby-sync.db "$LOCAL_TMP"

echo "== 予備機(CT $DST_CTID @ $STANDBY_HOST)へ転送"
ssh "$STANDBY_HOST" "pct exec $DST_CTID -- docker stop yomiage-tts >/dev/null 2>&1 || true"
scp -q "$LOCAL_TMP" "$STANDBY_HOST:$REMOTE_TMP"
ssh "$STANDBY_HOST" "pct push $DST_CTID $REMOTE_TMP $DEST/data/bot.db --perms 644 && \
    rm -f $REMOTE_TMP && \
    pct exec $DST_CTID -- rm -f $DEST/data/bot.db-wal $DEST/data/bot.db-shm && \
    pct exec $DST_CTID -- docker start yomiage-tts >/dev/null 2>&1 || true"

echo "== 完了 $(date -Iseconds)"
