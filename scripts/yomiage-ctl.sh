#!/bin/sh
# Proxmox ホスト上で yomiage-bot を管理するスクリプト。
#
#   pct / vzdump を使うので **Proxmox ホストで実行する**（LXC の中ではない）。
#   インストール例:
#     scp scripts/yomiage-ctl.sh root@<PVE>:/usr/local/bin/yomiage
#     ssh root@<PVE> chmod +x /usr/local/bin/yomiage
#
#   yomiage status            状態をまとめて表示
#   yomiage logs [行数|-f]    Bot のログ
#   yomiage engine-logs [N]   VOICEVOX ENGINE のログ
#   yomiage restart           Bot を再起動（VC からは自分で抜けてから落ちる）
#   yomiage start | stop
#   yomiage rebuild           イメージを作り直して差し替え
#   yomiage backup            SQLite を安全にバックアップ
#   yomiage prune             未使用イメージと古いビルドキャッシュを掃除
#   yomiage snapshot [名前]   LXC のスナップショットを取る
#   yomiage snapshots         スナップショット一覧
#   yomiage rollback <名前>   スナップショットに戻す（確認あり）
#   yomiage shell             コンテナに入る
#
# CTID などは環境変数で上書きできる。

set -eu

CTID="${CTID:-110}"
DEST="${DEST:-/opt/yomiage}"
BOT="${BOT:-yomiage-bot}"
ENGINE="${ENGINE:-voicevox}"
BACKUP_DIR="${BACKUP_DIR:-/var/backups/yomiage}"
KEEP_BACKUPS="${KEEP_BACKUPS:-14}"

die() {
    echo "エラー: $*" >&2
    exit 1
}

need_pct() {
    command -v pct >/dev/null 2>&1 ||
        die "pct が見つからない。このスクリプトは Proxmox ホストで実行する。"
}

need_running() {
    need_pct
    [ "$(pct status "$CTID" 2>/dev/null)" = "status: running" ] ||
        die "CT $CTID が起動していない。'pct start $CTID' で起動する。"
}

# コンテナ内で docker compose を叩く
compose() {
    pct exec "$CTID" -- sh -c "cd $DEST && docker compose $*"
}

cmd_status() {
    need_pct
    echo "== LXC $CTID"
    pct status "$CTID"
    pct config "$CTID" | grep -E '^(hostname|cores|memory|rootfs):' || true

    if [ "$(pct status "$CTID" 2>/dev/null)" != "status: running" ]; then
        echo
        echo "CT が停止中のため、これ以上は取得できない。"
        return 0
    fi

    echo
    echo "== コンテナ"
    pct exec "$CTID" -- docker ps -a \
        --format 'table {{.Names}}\t{{.Status}}\t{{.Image}}' 2>/dev/null || true

    echo
    echo "== ディスク"
    pct exec "$CTID" -- df -h / | tail -1
    used=$(pct exec "$CTID" -- df --output=pcent / | tail -1 | tr -dc '0-9')
    if [ "${used:-0}" -ge 80 ]; then
        echo "警告: 残りが少ない。'yomiage prune' で Docker のキャッシュを掃除できる。"
    fi

    echo
    echo "== 直近のログ"
    pct exec "$CTID" -- sh -c \
        "docker logs --tail 5 $BOT 2>&1 | grep -Ev 'serenity::|DAVE' || true"
}

cmd_logs() {
    need_running
    target="${1:-}"
    case "$target" in
        -f | --follow) pct exec "$CTID" -- docker logs -f --tail 50 "$BOT" ;;
        '') pct exec "$CTID" -- docker logs --tail 50 "$BOT" ;;
        *) pct exec "$CTID" -- docker logs --tail "$target" "$BOT" ;;
    esac
}

cmd_engine_logs() {
    need_running
    pct exec "$CTID" -- docker logs --tail "${1:-50}" "$ENGINE"
}

cmd_restart() {
    need_running
    # SIGTERM を受けると VC から抜けてから終了する。
    pct exec "$CTID" -- docker restart "$BOT" >/dev/null
    echo "再起動した。読み上げ対象の登録は消えるので /join からやり直すこと。"
}

cmd_start() {
    need_running
    compose "up -d $BOT"
}

cmd_stop() {
    need_running
    pct exec "$CTID" -- docker stop "$BOT" >/dev/null
    echo "停止した。"
}

cmd_rebuild() {
    need_running
    echo "== イメージを作り直す（数分かかる）"
    compose "up -d --build $BOT"
}

# SQLite は稼働中にファイルをコピーすると壊れうるので .backup を使う（PLAN §10.2）。
# CT に sqlite3 を入れたくないので、使い捨てコンテナで実行する。
cmd_backup() {
    need_running
    stamp=$(date +%Y%m%d-%H%M%S)
    pct exec "$CTID" -- mkdir -p "$BACKUP_DIR"
    pct exec "$CTID" -- docker run --rm \
        -v "$DEST/data:/data:ro" -v "$BACKUP_DIR:/backup" alpine:3 \
        sh -c "apk add --no-cache sqlite >/dev/null 2>&1 &&
               sqlite3 /data/bot.db \".backup '/backup/bot-$stamp.db'\""

    # 古いものを間引く
    pct exec "$CTID" -- sh -c "cd $BACKUP_DIR &&
        ls -1t bot-*.db 2>/dev/null | tail -n +$((KEEP_BACKUPS + 1)) | xargs -r rm -f"

    echo "== $BACKUP_DIR （CT 内）"
    pct exec "$CTID" -- sh -c "ls -lh $BACKUP_DIR | tail -5"
}

# ビルドキャッシュを全部消すと次のビルドで依存を再コンパイルして数分かかるので、
# 1 週間より古いものだけ落とす。
cmd_prune() {
    need_running
    echo "== 掃除前"
    pct exec "$CTID" -- df -h / | tail -1
    pct exec "$CTID" -- docker image prune -f
    pct exec "$CTID" -- docker builder prune -f --filter 'until=168h'
    echo "== 掃除後"
    pct exec "$CTID" -- df -h / | tail -1
}

cmd_snapshot() {
    need_pct
    name="${1:-auto-$(date +%Y%m%d-%H%M%S)}"
    # スナップショット前に DB を吸っておくと、戻したときの状態が揃う。
    if [ "$(pct status "$CTID" 2>/dev/null)" = "status: running" ]; then
        cmd_backup >/dev/null 2>&1 || echo "警告: バックアップに失敗した（続行する）" >&2
    fi
    pct snapshot "$CTID" "$name" --description "yomiage-ctl $(date -Iseconds)"
    echo "スナップショット '$name' を作成した。"
}

cmd_snapshots() {
    need_pct
    pct listsnapshot "$CTID"
}

cmd_rollback() {
    need_pct
    name="${1:?戻すスナップショット名を指定する（yomiage snapshots で一覧）}"
    echo "CT $CTID を '$name' に戻す。作成後の変更は失われる。"
    printf "続けるなら yes と入力: "
    read -r answer
    [ "$answer" = "yes" ] || die "中止した。"
    pct rollback "$CTID" "$name"
    echo "戻した。CT を起動するなら 'pct start $CTID'。"
}

cmd_shell() {
    need_running
    pct enter "$CTID"
}

usage() {
    sed -n '2,32p' "$0" | sed 's/^# \{0,1\}//'
}

case "${1:-}" in
    status) shift; cmd_status "$@" ;;
    logs) shift; cmd_logs "$@" ;;
    engine-logs) shift; cmd_engine_logs "$@" ;;
    restart) shift; cmd_restart "$@" ;;
    start) shift; cmd_start "$@" ;;
    stop) shift; cmd_stop "$@" ;;
    rebuild) shift; cmd_rebuild "$@" ;;
    backup) shift; cmd_backup "$@" ;;
    prune) shift; cmd_prune "$@" ;;
    snapshot) shift; cmd_snapshot "$@" ;;
    snapshots) shift; cmd_snapshots "$@" ;;
    rollback) shift; cmd_rollback "$@" ;;
    shell) shift; cmd_shell "$@" ;;
    '' | -h | --help | help) usage ;;
    *) die "不明なコマンド: $1（yomiage --help）" ;;
esac
