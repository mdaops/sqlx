#!/usr/bin/env bash
# Actual P3 HTTP component under wash, talking directly to PostgreSQL over P3 TCP.
set -euo pipefail
cd "$(dirname "$0")/.."
root="$PWD"
mkdir -p build/p3-wash/.wash
./scripts/build-p3.sh
container=""
wash_pid=""
cleanup() {
  if [[ -n "$wash_pid" ]]; then kill "$wash_pid" 2>/dev/null || true; wait "$wash_pid" 2>/dev/null || true; fi
  if [[ -n "$container" ]]; then docker rm -f "$container" >/dev/null; fi
}
trap cleanup EXIT
container="$(docker run -d --rm -e POSTGRES_HOST_AUTH_METHOD=trust -p 127.0.0.1::5432 "${POSTGRES_IMAGE:-postgres:16.14}")"
ready=false
for _ in {1..60}; do
  if docker exec "$container" pg_isready -U postgres >/dev/null 2>&1; then ready=true; break; fi
  sleep 1
done
if [[ "$ready" != true ]]; then docker logs "$container"; exit 1; fi
docker exec -i "$container" psql -U postgres -v ON_ERROR_STOP=1 < schema/schema.sql > build/p3-schema.log
port="$(docker port "$container" 5432/tcp)"
port="${port##*:}"
# Regression baseline: the same application APIs through the native runtime.
cargo build --locked -p sqlx-probe -p seaorm-probe > build/native-build.log 2>&1
DATABASE_URL="postgres://postgres@127.0.0.1:$port/postgres?sslmode=disable" ./target/debug/sqlx-probe
DATABASE_URL="postgres://postgres@127.0.0.1:$port/postgres?sslmode=disable" ./target/debug/seaorm-probe
http_port="${PROBE_HTTP_PORT:-18081}"
python3 - "$root" "$port" "$http_port" <<'PY'
import json, sys
root, port, http_port = sys.argv[1:]
config = {
  'build': {'command': 'true', 'component_path': root + '/target/wasm32-wasip1/release/p3_probe.wasm'},
  'dev': {'address': '127.0.0.1:' + http_port},
  'workload': {
    'allowedHosts': ['host.wasmcloud.internal:' + port],
    'allowedIpNameLookups': ['host.wasmcloud.internal'],
    'allowedHostLoopbackPorts': [int(port)],
    'environment': {'config': {'DATABASE_URL': 'postgres://postgres@host.wasmcloud.internal:' + port + '/postgres?sslmode=disable'}}
  }
}
with open(root + '/build/p3-wash/.wash/config.yaml', 'w') as f:
    json.dump(config, f, indent=2)
PY
wash -C "$root/build/p3-wash" dev --non-interactive > build/p3-wash.log 2>&1 &
wash_pid=$!
ready=false
for _ in {1..60}; do
  kill -0 "$wash_pid" || { echo 'wash exited: see build/p3-wash.log'; exit 1; }
  if curl -fsS --max-time 1 "http://127.0.0.1:$http_port/health" 2>/dev/null | grep -qx p3-ready; then ready=true; break; fi
  sleep 1
done
if [[ "$ready" != true ]]; then echo 'wash startup timed out: see build/p3-wash.log'; exit 1; fi
: > build/p3-instance-lifetime.txt
for _ in 1 2 3; do
  curl -fsS --max-time 5 "http://127.0.0.1:$http_port/instance" >> build/p3-instance-lifetime.txt
  printf '\n' >> build/p3-instance-lifetime.txt
done
for candidate in sqlx seaorm; do
  code="$(curl -sS --max-time 30 -o "build/p3-$candidate-response.txt" -w '%{http_code}' "http://127.0.0.1:$http_port/$candidate")"
  if [[ "$code" != 200 ]] || ! grep -q "PASS /$candidate:" "build/p3-$candidate-response.txt"; then
    echo "$candidate failed (HTTP $code): see build/p3-$candidate-response.txt and build/p3-wash.log"
    exit 1
  fi
  echo "PASS $candidate via actual P3 sockets: typed insert/read, NULL, rollback"
done
