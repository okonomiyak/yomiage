#!/bin/sh
# フェーズ 0（§11）の検証スクリプト。
# VOICEVOX ENGINE に疎通し、/audio_query → /synthesis で wav が取れることを確認する。
# 合わせて §7-5 の 48kHz / ステレオ指定が実際に wav ヘッダへ反映されるかも見る。
#
# 使い方:
#   docker compose -f compose.yaml -f compose.dev.yaml up -d voicevox
#   sh scripts/verify-engine.sh [speaker_id]
#
# 出力: ./out/verify-<speaker>.wav （再生して音が鳴ればフェーズ 0 完了）

set -eu

BASE="${VOICEVOX_URL:-http://localhost:50021}"
SPEAKER="${1:-3}"                      # 既定はずんだもん(ノーマル) = スタイル ID 3
TEXT="${TEXT:-ずんだもんなのだ。読み上げボットのテストなのだ。}"
OUTDIR="$(dirname "$0")/../out"
QUERY="$OUTDIR/query-$SPEAKER.json"
WAV="$OUTDIR/verify-$SPEAKER.wav"

mkdir -p "$OUTDIR"

say() { printf '\n== %s\n' "$1"; }

say "1. ヘルスチェック GET /version"
curl -fsS -m 5 "$BASE/version"
printf '\n'

say "2. 話者一覧 GET /speakers （先頭 20 行）"
# head だと grep 側が SIGPIPE で write error を出すので awk で打ち切る
curl -fsS -m 10 "$BASE/speakers" | tr ',' '\n' | grep -E '"(name|id)"' | awk 'NR<=20'

say "3. ウォームアップ POST /initialize_speaker?speaker=$SPEAKER"
start=$(date +%s)
curl -fsS -m 120 -X POST "$BASE/initialize_speaker?speaker=$SPEAKER" -o /dev/null
printf 'took %ss\n' "$(( $(date +%s) - start ))"

say "4. クエリ生成 POST /audio_query"
curl -fsS -m 30 -X POST \
  --get --data-urlencode "text=$TEXT" --data-urlencode "speaker=$SPEAKER" \
  "$BASE/audio_query" -o "$QUERY"
wc -c < "$QUERY" | tr -d ' ' | sed 's/^/query bytes: /'

say "5. 48kHz / ステレオへ書き換え（§7-5）"
sed -e 's/"outputSamplingRate": *[0-9]*/"outputSamplingRate":48000/' \
    -e 's/"outputStereo": *false/"outputStereo":true/' \
    "$QUERY" > "$QUERY.tmp" && mv "$QUERY.tmp" "$QUERY"
grep -o '"outputSamplingRate":[0-9]*' "$QUERY"
grep -o '"outputStereo":[a-z]*' "$QUERY"

say "6. 合成 POST /synthesis?speaker=$SPEAKER"
start=$(date +%s)
curl -fsS -m 120 -X POST -H 'Content-Type: application/json' \
  --data-binary @"$QUERY" "$BASE/synthesis?speaker=$SPEAKER" -o "$WAV"
printf 'took %ss\n' "$(( $(date +%s) - start ))"

say "7. wav ヘッダ検証"
head -c 4 "$WAV" | grep -q RIFF || { echo "NG: RIFF ヘッダが無い"; exit 1; }
# WAV fmt チャンク: 22-23 = チャンネル数, 24-27 = サンプリングレート（リトルエンディアン）
ch=$(od -An -tu2 -j22 -N2 "$WAV" | tr -d ' ')
rate=$(od -An -tu4 -j24 -N4 "$WAV" | tr -d ' ')
bytes=$(wc -c < "$WAV" | tr -d ' ')
printf 'channels=%s rate=%s bytes=%s\n' "$ch" "$rate" "$bytes"
[ "$ch" = "2" ] || { echo "NG: ステレオになっていない"; exit 1; }
[ "$rate" = "48000" ] || { echo "NG: 48000Hz になっていない"; exit 1; }

say "OK: $WAV を再生して音が鳴ればフェーズ 0 完了"
