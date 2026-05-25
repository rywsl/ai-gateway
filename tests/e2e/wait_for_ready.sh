#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
for i in $(seq 1 60); do
  if curl -sf http://localhost:8080/readyz > /dev/null; then
    echo "gateway ready"; exit 0
  fi
  sleep 1
done
echo "gateway did not become ready in 60s"
docker compose -f tests/e2e/docker-compose.yml logs --tail=200 gateway
exit 1
