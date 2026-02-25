#!/usr/bin/env bash
# client의 공개키를 server에 등록
set -euo pipefail

# client의 공개키 추출 → server에 주입
PUBKEY=$(docker exec qs-client cat /root/.ssh/id_ed25519.pub)
docker exec qs-server bash -c "echo '$PUBKEY' > /root/.ssh/authorized_keys && chmod 600 /root/.ssh/authorized_keys"

# client에서 known_hosts 등록 (StrictHostKeyChecking 우회)
docker exec qs-client bash -c "ssh-keyscan -H 172.30.0.10 >> /root/.ssh/known_hosts 2>/dev/null"

# 접속 테스트
docker exec qs-client ssh root@172.30.0.10 echo "SSH OK"
