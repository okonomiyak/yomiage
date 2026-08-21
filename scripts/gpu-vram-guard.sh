#!/bin/sh
# VOICEVOX (GPU版) の VRAM 使用率を監視し、閾値を超えたら voicevox コンテナを再起動する。
#
# GTX 1050 Ti は VRAM が 4GB しかなく、ENGINE は一度ロードした話者モデルを解放しない
# （run --help に unload/eviction 系のオプションが無い）。使われる話者が増えるほど
# VRAM を使い切り、合成が CUBLAS_STATUS_ALLOC_FAILED で失敗するようになる
# （2026-08-21 に発生、GPU化直後に確認）。再起動すれば VRAM は空に戻るので、
# 埋まりきる前に定期的に払う。
#
# LXC 110 内で cron から実行する想定:
#   */2 * * * * /opt/yomiage/scripts/gpu-vram-guard.sh >> /var/log/gpu-vram-guard.log 2>&1

set -eu

THRESHOLD_PERCENT="${GPU_VRAM_GUARD_THRESHOLD:-85}"
COMPOSE_DIR="${COMPOSE_DIR:-/opt/yomiage}"

OUT="$(nvidia-smi --query-gpu=memory.used,memory.total --format=csv,noheader,nounits)"
USED="$(echo "$OUT" | cut -d',' -f1 | tr -d ' ')"
TOTAL="$(echo "$OUT" | cut -d',' -f2 | tr -d ' ')"
PERCENT=$((USED * 100 / TOTAL))

echo "$(date -Is) VRAM ${USED}MiB/${TOTAL}MiB (${PERCENT}%)"

if [ "$PERCENT" -ge "$THRESHOLD_PERCENT" ]; then
    echo "$(date -Is) 閾値 ${THRESHOLD_PERCENT}% を超えたので voicevox を再起動します"
    (cd "$COMPOSE_DIR" && docker compose restart voicevox)
fi
