#!/bin/sh
# ニコニコの Cookie (NICO_COOKIES) を差し替えて疎通確認する。
#
#   sh scripts/update-nico-cookies.sh <エクスポートした cookies.txt>
#
# ブラウザ拡張機能（Get cookies.txt 等）で nicovideo.jp にログイン済みの状態から
# エクスポートした Netscape 形式の Cookie ファイルを渡すこと。
# 環境変数で上書きできる: PVE_HOST / CTID / DEST
#
# 再起動は不要。yt-dlp は呼び出しのたびにファイルを読むだけなので、
# 差し替えた次の再生から反映される。

set -eu

COOKIES_FILE="${1:?使い方: sh scripts/update-nico-cookies.sh <cookies.txt へのパス>}"
if [ ! -f "$COOKIES_FILE" ]; then
    echo "ファイルが見つからない: $COOKIES_FILE" >&2
    exit 1
fi

if [ -f .env ]; then
    PVE_HOST="${PVE_HOST:-$(sed -n 's/^PVE_HOST=//p' .env | tr -d '\r')}"
    CTID="${CTID:-$(sed -n 's/^CTID=//p' .env | tr -d '\r')}"
    DEST="${DEST:-$(sed -n 's/^DEST=//p' .env | tr -d '\r')}"
fi

PVE_HOST="${PVE_HOST:?PVE_HOST を .env に設定してください（例: root@10.0.0.1）}"
CTID="${CTID:?CTID を .env に設定してください（例: 110）}"
DEST="${DEST:-/opt/yomiage}"

REMOTE_TMP="/tmp/nico_cookies_$$.txt"

echo "== 転送 $COOKIES_FILE -> $PVE_HOST CT$CTID:$DEST/data/nico_cookies.txt"
scp "$COOKIES_FILE" "$PVE_HOST:$REMOTE_TMP"
# 差し替える前に今のファイルを 1 世代だけ退避する。差し替えた内容が壊れていた場合に
# 手で戻せるようにするため（新しいものを試すたびに前のものが消えると戻せない）。
ssh "$PVE_HOST" "pct exec $CTID -- sh -c 'test -f $DEST/data/nico_cookies.txt && cp $DEST/data/nico_cookies.txt $DEST/data/nico_cookies.txt.bak || true'"
ssh "$PVE_HOST" "pct push $CTID $REMOTE_TMP $DEST/data/nico_cookies.txt --perms 600 && rm -f $REMOTE_TMP"

echo "== 疎通確認（コピーで確認するので置いた原本には触れない）"
echo "   動画のメタデータ (JSON) が出れば成功。"
echo "   'ERROR: ... Invalid session' が出たら、ログインし直してエクスポートし直すこと。"
# sm9 はニコニコの定番の公開テスト動画。会員限定かどうかに関係なく疎通確認に使える。
ssh "$PVE_HOST" "pct exec $CTID -- sh -c '\
    cp $DEST/data/nico_cookies.txt /tmp/nico_verify.txt && \
    docker cp /tmp/nico_verify.txt yomiage-bot:/tmp/nico_verify.txt && \
    docker exec yomiage-bot yt-dlp --no-playlist --no-warnings -j --cookies /tmp/nico_verify.txt https://www.nicovideo.jp/watch/sm9; \
    docker exec yomiage-bot rm -f /tmp/nico_verify.txt; \
    rm -f /tmp/nico_verify.txt'"
