#!/bin/sh
# yomiage-bot の運用スクリプト。
#
#   Proxmox ホストでも LXC の中でも動く（pct があればホスト、無ければ CT 内と判断する）。
#   スナップショット系だけはホスト専用。
#
#   インストール:
#     ホスト: scp scripts/yomiage-ctl.sh root@<PVE>:/usr/local/bin/yomiage
#     CT 内 : /opt/yomiage/scripts/yomiage-ctl.sh を /usr/local/bin/yomiage へコピー
#
#   yomiage status                 状態をまとめて表示
#   yomiage logs [tts|music] [行数|-f]    Bot のログ（対象省略で両方）
#   yomiage engine-logs [N]        VOICEVOX ENGINE のログ
#   yomiage restart [tts|music]    Bot を再起動（対象省略で両方。VC からは自分で抜けてから落ちる）
#   yomiage start | stop [tts|music]
#   yomiage rebuild [tts|music]    イメージを作り直して差し替え（対象省略で両方）
#   yomiage backup            SQLite を安全にバックアップ
#   yomiage prune             未使用イメージと古いビルドキャッシュを掃除
#   yomiage snapshot [名前]   LXC のスナップショットを取る（ホスト専用）
#   yomiage snapshots         スナップショット一覧（ホスト専用）
#   yomiage rollback <名前>   スナップショットに戻す（ホスト専用・確認あり）
#   yomiage shell             コンテナに入る（ホスト専用）
#
# 読み上げ(tts-bot)と音楽(music-bot)は別プロセス・別 Discord アプリ（PLAN §13）。
# status/logs/restart/start/stop/rebuild は対象を省略すると両方に対して行う。
#
# CTID などは環境変数で上書きできる。

set -eu

CTID="${CTID:-110}"
DEST="${DEST:-/opt/yomiage}"
BOT_TTS="${BOT_TTS:-yomiage-tts}"
BOT_MUSIC="${BOT_MUSIC:-yomiage-music}"
ENGINE="${ENGINE:-voicevox}"
BACKUP_DIR="${BACKUP_DIR:-/var/backups/yomiage}"
KEEP_BACKUPS="${KEEP_BACKUPS:-14}"

# "tts" / "music" / "" (両方) → コンテナ名（1 個 or 2 個、空白区切り）。
bot_names() {
    case "${1:-}" in
        tts) echo "$BOT_TTS" ;;
        music) echo "$BOT_MUSIC" ;;
        '') echo "$BOT_TTS $BOT_MUSIC" ;;
        *) die "対象は tts か music を指定する（省略で両方）。" ;;
    esac
}

# "tts" / "music" / "" (両方) → compose のサービス名。
service_names() {
    case "${1:-}" in
        tts) echo "tts-bot" ;;
        music) echo "music-bot" ;;
        '') echo "tts-bot music-bot" ;;
        *) die "対象は tts か music を指定する（省略で両方）。" ;;
    esac
}

if command -v pct >/dev/null 2>&1; then
    MODE=host
else
    MODE=ct
fi

die() {
    echo "エラー: $*" >&2
    exit 1
}

need_host() {
    [ "$MODE" = host ] ||
        die "このコマンドは Proxmox ホストで実行する（pct が要る）。"
}

# 対象コマンドを CT 内で実行する。ホストからなら pct exec 経由、CT 内ならそのまま。
in_ct() {
    if [ "$MODE" = host ]; then
        pct exec "$CTID" -- "$@"
    else
        "$@"
    fi
}

need_running() {
    if [ "$MODE" = host ]; then
        [ "$(pct status "$CTID" 2>/dev/null)" = "status: running" ] ||
            die "CT $CTID が起動していない。'pct start $CTID' で起動する。"
    else
        command -v docker >/dev/null 2>&1 ||
            die "docker が見つからない。CT 内か Proxmox ホストで実行する。"
    fi
}

compose() {
    in_ct sh -c "cd $DEST && docker compose $*"
}

cmd_status() {
    if [ "$MODE" = host ]; then
        echo "== LXC $CTID"
        pct status "$CTID"
        pct config "$CTID" | grep -E '^(hostname|cores|memory|rootfs):' || true

        if [ "$(pct status "$CTID" 2>/dev/null)" != "status: running" ]; then
            echo
            echo "CT が停止中のため、これ以上は取得できない。"
            return 0
        fi
        echo
    fi

    echo "== コンテナ"
    in_ct docker ps -a --format 'table {{.Names}}\t{{.Status}}\t{{.Image}}' 2>/dev/null || true

    echo
    echo "== ディスク"
    in_ct df -h / | tail -1
    used=$(in_ct df --output=pcent / | tail -1 | tr -dc '0-9')
    if [ "${used:-0}" -ge 80 ]; then
        echo "警告: 残りが少ない。'yomiage prune' で Docker のキャッシュを掃除できる。"
    fi

    echo
    echo "== 直近のログ"
    for name in $(bot_names); do
        echo "-- $name"
        in_ct sh -c "docker logs --tail 5 $name 2>&1 | grep -Ev 'serenity::|DAVE' || true"
    done
}

cmd_logs() {
    need_running
    target=""
    case "${1:-}" in
        tts | music)
            target="$1"
            shift
            ;;
    esac
    case "${1:-}" in
        -f | --follow)
            [ -n "$target" ] || die "-f で追うときは対象（tts か music）を指定する。"
            in_ct docker logs -f --tail 50 "$(bot_names "$target")"
            ;;
        '')
            for name in $(bot_names "$target"); do
                echo "== $name"
                in_ct docker logs --tail 50 "$name"
            done
            ;;
        *)
            for name in $(bot_names "$target"); do
                echo "== $name"
                in_ct docker logs --tail "$1" "$name"
            done
            ;;
    esac
}

cmd_engine_logs() {
    need_running
    in_ct docker logs --tail "${1:-50}" "$ENGINE"
}

cmd_restart() {
    need_running
    # SIGTERM を受けると VC から抜けてから終了する。
    for name in $(bot_names "${1:-}"); do
        in_ct docker restart "$name" >/dev/null
    done
    echo "再起動した。読み上げ対象の登録は消えるので /join からやり直すこと。"
}

cmd_start() {
    need_running
    compose "up -d $(service_names "${1:-}")"
}

cmd_stop() {
    need_running
    for name in $(bot_names "${1:-}"); do
        in_ct docker stop "$name" >/dev/null
    done
    echo "停止した。"
}

cmd_rebuild() {
    need_running
    echo "== イメージを作り直す（数分かかる）"
    compose "up -d --build $(service_names "${1:-}")"
}

# SQLite は稼働中にファイルをコピーすると壊れうるので .backup を使う（PLAN §10.2）。
# CT に sqlite3 を入れたくないので、使い捨てコンテナで実行する。
cmd_backup() {
    need_running
    stamp=$(date +%Y%m%d-%H%M%S)
    in_ct mkdir -p "$BACKUP_DIR"
    in_ct docker run --rm \
        -v "$DEST/data:/data:ro" -v "$BACKUP_DIR:/backup" alpine:3 \
        sh -c "apk add --no-cache sqlite >/dev/null 2>&1 &&
               sqlite3 /data/bot.db \".backup '/backup/bot-$stamp.db'\""

    # 古いものを間引く
    in_ct sh -c "cd $BACKUP_DIR &&
        ls -1t bot-*.db 2>/dev/null | tail -n +$((KEEP_BACKUPS + 1)) | xargs -r rm -f"

    echo "== $BACKUP_DIR"
    in_ct sh -c "ls -lh $BACKUP_DIR | tail -5"
}

# ビルドキャッシュを全部消すと次のビルドで依存を再コンパイルして数分かかるので、
# 1 週間より古いものだけ落とす。
cmd_prune() {
    need_running
    echo "== 掃除前"
    in_ct df -h / | tail -1
    in_ct docker image prune -f
    in_ct docker builder prune -f --filter 'until=168h'
    echo "== 掃除後"
    in_ct df -h / | tail -1
}

cmd_snapshot() {
    need_host
    name="${1:-auto-$(date +%Y%m%d-%H%M%S)}"
    # スナップショット前に DB を吸っておくと、戻したときの状態が揃う。
    if [ "$(pct status "$CTID" 2>/dev/null)" = "status: running" ]; then
        cmd_backup >/dev/null 2>&1 || echo "警告: バックアップに失敗した（続行する）" >&2
    fi
    pct snapshot "$CTID" "$name" --description "yomiage-ctl $(date -Iseconds)"
    echo "スナップショット '$name' を作成した。"
}

cmd_snapshots() {
    need_host
    pct listsnapshot "$CTID"
}

cmd_rollback() {
    need_host
    name="${1:?戻すスナップショット名を指定する（yomiage snapshots で一覧）}"
    echo "CT $CTID を '$name' に戻す。作成後の変更は失われる。"
    printf "続けるなら yes と入力: "
    read -r answer
    [ "$answer" = "yes" ] || die "中止した。"
    pct rollback "$CTID" "$name"
    echo "戻した。CT を起動するなら 'pct start $CTID'。"
}

cmd_shell() {
    need_host
    pct enter "$CTID"
}

usage() {
    sed -n '2,27p' "$0" | sed 's/^# \{0,1\}//'
    echo "現在のモード: $MODE（host = Proxmox ホスト / ct = コンテナ内）"
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
