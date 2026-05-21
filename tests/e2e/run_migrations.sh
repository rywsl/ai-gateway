#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
for f in crates/gw-store/migrations/*.sql; do
  echo "applying $f"
  docker compose -f tests/e2e/docker-compose.yml exec -T postgres psql -U postgres -d gateway -f - < "$f"
done
