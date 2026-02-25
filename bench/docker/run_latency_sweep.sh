#!/usr/bin/env bash
# Docker 컨테이너 간 지연 스윕 벤치마크
#
# 사용법: ./bench/docker/run_latency_sweep.sh [반복횟수]
#
# 전체 실행 (빌드부터):
#   docker compose -f bench/docker/compose.yml up -d --build
#   bash bench/docker/setup_ssh.sh
#   bash bench/docker/run_latency_sweep.sh 3
#   docker compose -f bench/docker/compose.yml down

set -euo pipefail

ROUNDS="${1:-3}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CSV="$(dirname "$SCRIPT_DIR")/latency_sweep.csv"
SERVER="root@172.30.0.10"
SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null"
DELAYS=(0 25 50 100 250)  # 단방향 ms → RTT = 2x
DATA_MB=128
DATA_BYTES=$((DATA_MB * 1048576))

echo "=== 지연 스윕 벤치마크 ==="
echo "RTT: ${DELAYS[*]} (단방향 ms)"
echo "반복: $ROUNDS, 데이터: ${DATA_MB}MB"
echo ""

# 컨테이너 동작 확인
docker exec qs-client true 2>/dev/null || { echo "오류: qs-client 컨테이너가 없습니다. compose up 먼저 실행하세요." >&2; exit 1; }

# 테스트 데이터 생성
echo "테스트 데이터 생성..."
docker exec qs-client bash -c "dd if=/dev/urandom of=/tmp/bench/test.bin bs=1M count=$DATA_MB 2>/dev/null"

# ── 지연 설정 (pumba) ─────────────────────────────────
# pumba로 양방향 지연을 주입한다.
# client→server, server→client 모두에 delay를 걸어야 실제 WAN RTT를 재현할 수 있다.
# pumba는 helper container로 타겟의 네트워크 네임스페이스에 tc netem을 주입한다.
PUMBA_IMAGE="ghcr.io/alexei-led/pumba:latest"

clear_delay() {
    # 기존 pumba 프로세스 종료 (pumba가 tc 규칙을 자동 정리)
    docker ps -q --filter "ancestor=$PUMBA_IMAGE" | xargs -r docker rm -f 2>/dev/null || true
    sleep 2
    # pumba 정리가 불완전할 수 있으므로 양쪽 모두 직접 제거
    docker exec qs-client tc qdisc del dev eth0 root 2>/dev/null || true
    docker exec qs-server tc qdisc del dev eth0 root 2>/dev/null || true
}

set_delay() {
    local ms="$1"
    clear_delay
    sleep 1
    if [[ "$ms" -gt 0 ]]; then
        # client에 지연 (client→server 방향)
        docker run -d --rm \
            -v /var/run/docker.sock:/var/run/docker.sock \
            "$PUMBA_IMAGE" \
            netem --duration 30m --interface eth0 \
            delay --time "$ms" --jitter 0 \
            qs-client >/dev/null 2>&1
        # server에 지연 (server→client 방향)
        docker run -d --rm \
            -v /var/run/docker.sock:/var/run/docker.sock \
            "$PUMBA_IMAGE" \
            netem --duration 30m --interface eth0 \
            delay --time "$ms" --jitter 0 \
            qs-server >/dev/null 2>&1
        sleep 2  # pumba가 tc 규칙을 적용할 시간
    fi
    # RTT 확인
    local rtt
    rtt=$(docker exec qs-client ping -c 2 -W 2 172.30.0.10 2>/dev/null | grep 'time=' | tail -1 | grep -o 'time=[0-9.]*' | cut -d= -f2 || echo "?")
    echo "   지연 ${ms}ms 설정 (측정 RTT: ${rtt}ms)"
}

# ── 측정 ──────────────────────────────────────────────
run_one() {
    local tool="$1" delay_ms="$2" round="$3"
    local remote_dir="/app/upload-test/${tool}"

    # 원격 초기화
    docker exec qs-client ssh $SSH_OPTS $SERVER "rm -rf $remote_dir && mkdir -p $remote_dir" 2>/dev/null

    local wall_sec
    case "$tool" in
        rsync_ssh)
            wall_sec=$(docker exec qs-client bash -c "
                START=\$(date +%s.%N)
                rsync -a /tmp/bench/test.bin $SERVER:$remote_dir/ >/dev/null 2>&1
                END=\$(date +%s.%N)
                echo \"\$END - \$START\" | bc -l
            ")
            ;;
        quicsync)
            wall_sec=$(docker exec qs-client bash -c "
                START=\$(date +%s.%N)
                quicsync -a /tmp/bench/test.bin $SERVER:$remote_dir/ >/dev/null 2>&1
                END=\$(date +%s.%N)
                echo \"\$END - \$START\" | bc -l
            ")
            ;;
    esac

    local rtt_ms=$((delay_ms * 2))
    local throughput="0"
    if [[ "$(echo "$wall_sec > 0" | bc -l 2>/dev/null || echo 0)" == "1" ]]; then
        throughput=$(echo "scale=2; $DATA_BYTES / 1048576 / $wall_sec" | bc -l)
    fi

    echo "${tool},${rtt_ms},${round},${wall_sec},${throughput}"
}

# ── 메인 ──────────────────────────────────────────────
echo "tool,rtt_ms,round,wall_sec,throughput_mbps" > "$CSV"

for delay in "${DELAYS[@]}"; do
    rtt=$((delay * 2))
    echo ""
    echo "── RTT ${rtt}ms (delay ${delay}ms) ──"
    set_delay "$delay"

    for round in $(seq 1 "$ROUNDS"); do
        for tool in rsync_ssh quicsync; do
            printf "   %-12s #%d ... " "$tool" "$round"
            row=$(run_one "$tool" "$delay" "$round")
            echo "$row" >> "$CSV"

            wall=$(echo "$row" | cut -d, -f4)
            tp=$(echo "$row" | cut -d, -f5)
            printf "%6ss  %s MB/s\n" "$wall" "$tp"
        done
    done
done

# 지연 제거
clear_delay

echo ""
echo "=== 결과: $CSV ==="
echo ""

# ── 요약 ──────────────────────────────────────────────
echo "=== 요약 (RTT별 평균) ==="
printf "%-14s %8s %10s %12s %10s\n" "tool" "RTT(ms)" "wall(s)" "MB/s" "배수"
echo "────────────────────────────────────────────────────────────"

tail -n +2 "$CSV" | awk -F, '
{
    key = $1 SUBSEP $2
    wall[key] += $4; tp[key] += $5; cnt[key]++
}
END {
    # rsync_ssh 기준 배수 계산을 위해 먼저 rsync 값 저장
    for (k in cnt) {
        split(k, a, SUBSEP)
        avg_wall[a[1],a[2]] = wall[k]/cnt[k]
        avg_tp[a[1],a[2]] = tp[k]/cnt[k]
    }
    # RTT 순서대로 출력
    n = split("0 50 100 200 500", rtts, " ")
    for (i = 1; i <= n; i++) {
        r = rtts[i]
        rw = avg_wall["rsync_ssh",r]
        qw = avg_wall["quicsync",r]
        if (rw > 0 && qw > 0) {
            ratio = sprintf("%.2fx", rw / qw)
            printf "%-14s %8s %10.2f %12.2f %10s\n", "rsync_ssh", r, rw, avg_tp["rsync_ssh",r], "1.00x"
            printf "%-14s %8s %10.2f %12.2f %10s\n", "quicsync", r, qw, avg_tp["quicsync",r], ratio
            print ""
        }
    }
}'
