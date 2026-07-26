#!/bin/sh
# コミット済みのツリーを LXC へ送ってビルド・再起動する。
#
#   sh scripts/deploy.sh
#
# 環境変数で上書きできる: PVE_HOST / CTID / DEST
# 事前に LXC の $DEST/.env に DISCORD_TOKEN を置いておくこと（このスクリプトは触らない）。

set -eu

# 接続先は .env（git 管理外）から読む。公開リポジトリに自宅サーバーのアドレスを
# 置かないため。環境変数で直接渡してもよい。
if [ -f .env ]; then
    PVE_HOST="${PVE_HOST:-$(sed -n 's/^PVE_HOST=//p' .env | tr -d '\r')}"
    CTID="${CTID:-$(sed -n 's/^CTID=//p' .env | tr -d '\r')}"
    DEST="${DEST:-$(sed -n 's/^DEST=//p' .env | tr -d '\r')}"
fi

PVE_HOST="${PVE_HOST:?PVE_HOST を .env に設定してください（例: root@10.0.0.1）}"
CTID="${CTID:?CTID を .env に設定してください（例: 110）}"
DEST="${DEST:-/opt/yomiage}"

if ! git diff --quiet HEAD 2>/dev/null; then
    echo "警告: コミットしていない変更がある。送られるのは HEAD の内容だけ。" >&2
fi

echo "== 転送 $(git rev-parse --short HEAD) -> $PVE_HOST CT$CTID:$DEST"
# git archive なので作業ツリーの中途半端な状態は混ざらない。.env や data/ も送らない。
git archive --format=tar HEAD | ssh "$PVE_HOST" "pct exec $CTID -- tar -x -C $DEST"

echo "== ビルドと再起動"
ssh "$PVE_HOST" "pct exec $CTID -- sh -c 'cd $DEST && docker compose up -d --build yomiage-bot'"

echo "== 状態"
ssh "$PVE_HOST" "pct exec $CTID -- sh -c 'cd $DEST && docker compose ps'"

echo
echo "ログ: ssh $PVE_HOST \"pct exec $CTID -- docker logs -f yomiage-bot\""
