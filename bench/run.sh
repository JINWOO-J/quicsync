#!/usr/bin/env bash
# quicsync vs rsync+ssh 성능 비교 벤치마크
#
# 사용법: ./bench/run.sh user@host:/remote/path [반복횟수]
# 예시:   ./bench/run.sh root@100.67.177.67:/app/upload-test/ 3
#
# 필요:
#   - quicsync (make install)
#   - rsync
#   - gtime (brew install gnu-time)
#   - 원격 SSH 접속 가능

set -euo pipefail

# ── 인수 & 설정 ──────────────────────────────────────
REMOTE="${1:?사용법: $0 user@host:/remote/path [반복횟수]}"
ROUNDS="${2:-3}"
BENCH_DIR="$(cd "$(dirname "$0")" && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/quicsync-bench.XXXXXX")"
CSV="$BENCH_DIR/results.csv"

# gtime 필수 확인
if ! command -v gtime &>/dev/null; then
    echo "오류: gtime이 필요합니다. brew install gnu-time" >&2
    exit 1
fi

# 원격 경로 파싱: user@host:/path → SSH_TARGET, REMOTE_BASE
if [[ "$REMOTE" =~ ^([^:]+):(.+)$ ]]; then
    SSH_TARGET="${BASH_REMATCH[1]}"
    REMOTE_BASE="${BASH_REMATCH[2]%/}"
else
    echo "오류: 원격 경로 형식 오류 (user@host:/path)" >&2
    exit 1
fi

echo "=== quicsync 벤치마크 ==="
echo "원격: $REMOTE"
echo "반복: $ROUNDS 회"
echo "임시: $WORK_DIR"
echo ""

cleanup() { rm -rf "$WORK_DIR"; }
trap cleanup EXIT

# ── 테스트 데이터 생성 ────────────────────────────────
generate_data() {
    local scenario="$1" dir="$WORK_DIR/$scenario"
    mkdir -p "$dir"
    case "$scenario" in
        single_large)
            # 256MB 단일 파일
            dd if=/dev/urandom of="$dir/large.bin" bs=1m count=256 2>/dev/null ;;
        many_small)
            # 1000개 × 100KB
            mkdir -p "$dir/files"
            for i in $(seq 1 1000); do
                dd if=/dev/urandom of="$dir/files/f_$(printf '%04d' $i).dat" \
                   bs=1024 count=100 2>/dev/null
            done ;;
        mixed)
            # 64MB + 32MB + 200개 × 10KB
            dd if=/dev/urandom of="$dir/big1.bin" bs=1m count=64 2>/dev/null
            dd if=/dev/urandom of="$dir/big2.bin" bs=1m count=32 2>/dev/null
            mkdir -p "$dir/small"
            for i in $(seq 1 200); do
                dd if=/dev/urandom of="$dir/small/s_$(printf '%03d' $i).txt" \
                   bs=1024 count=10 2>/dev/null
            done ;;
        incremental)
            # mixed와 동일 (초기 전송 후 일부 변경)
            dd if=/dev/urandom of="$dir/big1.bin" bs=1m count=64 2>/dev/null
            dd if=/dev/urandom of="$dir/big2.bin" bs=1m count=32 2>/dev/null
            mkdir -p "$dir/small"
            for i in $(seq 1 200); do
                dd if=/dev/urandom of="$dir/small/s_$(printf '%03d' $i).txt" \
                   bs=1024 count=10 2>/dev/null
            done ;;
    esac
    du -sh "$dir" | cut -f1
}

apply_incremental_changes() {
    local dir="$WORK_DIR/incremental"
    # 큰 파일 마지막 1MB 변경
    dd if=/dev/urandom of="$dir/big1.bin" bs=1m count=1 seek=63 conv=notrunc 2>/dev/null
    # 작은 파일 10개 변경 + 5개 추가
    for i in $(seq 1 10); do
        dd if=/dev/urandom of="$dir/small/s_$(printf '%03d' $i).txt" \
           bs=1024 count=10 2>/dev/null
    done
    for i in $(seq 201 205); do
        dd if=/dev/urandom of="$dir/small/s_$(printf '%03d' $i).txt" \
           bs=1024 count=10 2>/dev/null
    done
}

# ── 측정 ──────────────────────────────────────────────
# run_bench <tool> <scenario> <round> <src_path>
# stdout: CSV 행
run_bench() {
    local tool="$1" scenario="$2" round="$3" src="$4"
    local time_out="$WORK_DIR/.time.txt"
    local all_out="$WORK_DIR/.out.txt"
    local remote_dir="${REMOTE_BASE}/bench_${tool}_${scenario}"

    # 원격 디렉토리 준비 (incremental round>1 제외)
    if [[ "$scenario" != "incremental" ]] || [[ "$round" == "1" ]]; then
        ssh "$SSH_TARGET" "rm -rf $remote_dir && mkdir -p $remote_dir" 2>/dev/null
    fi

    # gtime -o 로 time 출력을 별도 파일에 저장
    # rsync/quicsync의 stdout+stderr는 all_out에 합침
    case "$tool" in
        rsync_ssh)
            gtime -f '%e %U %S %M' -o "$time_out" \
                rsync -a --stats "$src/" "${SSH_TARGET}:${remote_dir}/" \
                > "$all_out" 2>&1 || true
            ;;
        quicsync)
            gtime -f '%e %U %S %M' -o "$time_out" \
                quicsync -a "$src/" "${SSH_TARGET}:${remote_dir}/" \
                > "$all_out" 2>&1 || true
            ;;
    esac

    # gtime 파싱: "real_sec user_sec sys_sec maxrss_kb"
    local tl
    tl=$(tail -1 "$time_out" 2>/dev/null || echo "0 0 0 0")
    local wall_sec user_sec sys_sec maxrss_kb
    wall_sec=$(echo "$tl" | awk '{print $1}')
    user_sec=$(echo "$tl" | awk '{print $2}')
    sys_sec=$(echo "$tl" | awk '{print $3}')
    maxrss_kb=$(echo "$tl" | awk '{print $4}')

    # CPU% 계산
    local cpu_pct="0"
    if [[ "$(echo "$wall_sec > 0" | bc -l)" == "1" ]]; then
        cpu_pct=$(echo "scale=1; ($user_sec + $sys_sec) / $wall_sec * 100" | bc -l)
    fi

    # --stats 출력에서 sent/received bytes 파싱
    local sent_bytes="0" recv_bytes="0"
    if grep -q 'sent.*bytes.*received' "$all_out" 2>/dev/null; then
        local stats_line
        stats_line=$(grep 'sent.*bytes.*received' "$all_out" | head -1 | tr -d ',')
        sent_bytes=$(echo "$stats_line" | awk '{print $2}')
        recv_bytes=$(echo "$stats_line" | awk '{print $5}')
    fi

    # throughput MB/s
    local throughput="0"
    if [[ "$(echo "$wall_sec > 0" | bc -l)" == "1" ]] && [[ "$sent_bytes" != "0" ]]; then
        throughput=$(echo "scale=2; $sent_bytes / 1048576 / $wall_sec" | bc -l)
    fi

    echo "${tool},${scenario},${round},${wall_sec},${user_sec},${sys_sec},${cpu_pct},${maxrss_kb},${sent_bytes},${recv_bytes},${throughput}"
}

# ── 메인 실행 ─────────────────────────────────────────
HEADER="tool,scenario,round,wall_sec,user_sec,sys_sec,cpu_pct,maxrss_kb,sent_bytes,recv_bytes,throughput_mbps"
echo "$HEADER" > "$CSV"

for scenario in single_large many_small mixed incremental; do
    echo "── 데이터 생성: $scenario ──"
    data_size=$(generate_data "$scenario")
    echo "   크기: $data_size"

    for round in $(seq 1 "$ROUNDS"); do
        if [[ "$scenario" == "incremental" && "$round" -gt 1 ]]; then
            echo "   [incremental] 파일 변경 적용..."
            apply_incremental_changes
        fi

        for tool in rsync_ssh quicsync; do
            printf "   %-12s %-14s #%d ... " "$tool" "$scenario" "$round"
            row=$(run_bench "$tool" "$scenario" "$round" "$WORK_DIR/$scenario")
            echo "$row" >> "$CSV"

            wall=$(echo "$row" | cut -d, -f4)
            rss=$(echo "$row" | cut -d, -f8)
            tp=$(echo "$row" | cut -d, -f11)
            printf "%6ss  RSS:%sKB  %sMB/s\n" "$wall" "$rss" "$tp"
        done
    done
    echo ""
done

echo "=== 결과: $CSV ==="
echo ""

# ── 요약 리포트 ──────────────────────────────────────
echo "=== 요약 (시나리오별 평균) ==="
printf "%-14s %-14s %10s %10s %8s %10s %12s\n" \
    "tool" "scenario" "wall(s)" "cpu(%)" "RSS(KB)" "sent(B)" "MB/s"
echo "──────────────────────────────────────────────────────────────────────────────"

# CSV에서 시나리오별 평균 계산 (awk)
tail -n +2 "$CSV" | awk -F, '
{
    key = $1 SUBSEP $2
    wall[key] += $4; cpu[key] += $7; rss[key] += $8
    sent[key] += $9; tp[key] += $11; cnt[key]++
}
END {
    for (k in cnt) {
        split(k, a, SUBSEP)
        printf "%-14s %-14s %10.2f %10.1f %8d %10d %12.2f\n",
            a[1], a[2],
            wall[k]/cnt[k], cpu[k]/cnt[k], rss[k]/cnt[k],
            sent[k]/cnt[k], tp[k]/cnt[k]
    }
}' | sort -k2,2 -k1,1

echo ""
echo "완료. 상세 데이터: $CSV"
