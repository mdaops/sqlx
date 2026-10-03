#!/usr/bin/env bash
# Local-only live tests. All keys and generated configuration stay under ignored build/.
set -euo pipefail
umask 077
cd "$(dirname "$0")/.."
./scripts/build-p3.sh
root="$PWD"
out="$root/build/hardening"
mkdir -p "$out/certs" "$out/wash/.wash"
certs="$out/certs"
container=""
wash_pid=""
cleanup() {
  if [[ -n "$wash_pid" ]]; then kill "$wash_pid" 2>/dev/null || true; wait "$wash_pid" 2>/dev/null || true; fi
  if [[ -n "$container" ]]; then
    docker logs "$container" > "$out/postgres.log" 2>&1 || true
    docker rm -f "$container" >/dev/null
  fi
}
trap cleanup EXIT
openssl req -x509 -newkey rsa:2048 -nodes -keyout "$certs/ca.key" -out "$certs/ca.crt" \
  -days 2 -subj '/CN=interop-local-test-ca' > "$out/openssl.log" 2>&1
openssl req -x509 -newkey rsa:2048 -nodes -keyout "$certs/wrong-ca.key" -out "$certs/wrong-ca.crt" \
  -days 2 -subj '/CN=untrusted-test-ca' >> "$out/openssl.log" 2>&1
openssl req -new -newkey rsa:2048 -nodes -keyout "$certs/server.key" -out "$certs/server.csr" \
  -subj '/CN=host.wasmcloud.internal' >> "$out/openssl.log" 2>&1
for variant in good wrong-name expired; do
  name=host.wasmcloud.internal
  days=2
  if [[ "$variant" == wrong-name ]]; then name=wrong.example.invalid; fi
  if [[ "$variant" == expired ]]; then days=0; fi
  printf 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:%s\n' "$name" > "$certs/leaf.ext"
  openssl x509 -req -in "$certs/server.csr" -CA "$certs/ca.crt" -CAkey "$certs/ca.key" \
    -set_serial "$((100 + days + ${#variant}))" -days "$days" -extfile "$certs/leaf.ext" \
    -out "$certs/$variant.crt" >> "$out/openssl.log" 2>&1
done

container="$(docker run -d --rm -e POSTGRES_HOST_AUTH_METHOD=trust -p 127.0.0.1::5432 "${POSTGRES_IMAGE:-postgres:16.14}")"
ready=false
for _ in {1..60}; do
  if docker exec "$container" pg_isready -U postgres >/dev/null 2>&1; then ready=true; break; fi
  sleep 1
done
if [[ "$ready" != true ]]; then echo 'Postgres startup failed'; exit 1; fi
docker exec -i "$container" psql -U postgres -v ON_ERROR_STOP=1 < schema/schema.sql > "$out/schema.log"
docker exec -u 0 "$container" mkdir /tmp/interop-tls
docker cp "$certs/server.key" "$container:/tmp/interop-tls/server.key" >/dev/null
docker exec -u 0 "$container" chown postgres:postgres /tmp/interop-tls/server.key
docker exec -u 0 "$container" chmod 600 /tmp/interop-tls/server.key
# Certificates are public; copying them readable lets the postgres user load them.
chmod 644 "$certs/"*.crt
install_cert() {
  docker cp "$certs/$1.crt" "$container:/tmp/interop-tls/server.crt" >/dev/null
  docker exec "$container" psql -U postgres -v ON_ERROR_STOP=1 -c 'SELECT pg_reload_conf()' >/dev/null
  sleep 1
}
install_cert good
for setting in "ssl=on" "ssl_cert_file=/tmp/interop-tls/server.crt" "ssl_key_file=/tmp/interop-tls/server.key"; do
  docker exec "$container" psql -U postgres -v ON_ERROR_STOP=1 \
    -c "ALTER SYSTEM SET ${setting%%=*} = '${setting#*=}'" >/dev/null
done
docker exec "$container" psql -U postgres -c 'SELECT pg_reload_conf()' >/dev/null
sleep 1
port="$(docker port "$container" 5432/tcp)"; port="${port##*:}"
http_port="${PROBE_HTTP_PORT:-18082}"
wasm-tools parse fixtures/empty.wat -o "$out/empty.wasm"
python3 - "$root" "$out" "$port" "$http_port" <<'PY'
import json, sys
root, out, port, http_port = sys.argv[1:]
config = {
 'build': {'command': 'true', 'component_path': out + '/empty.wasm'},
 'dev': {
   'address': '127.0.0.1:' + http_port,
   'volumes': [{'host_path': out + '/certs', 'guest_path': '/certs'}],
   'components': [{
     'name': 'postgres-probe', 'file': root + '/target/wasm32-wasip1/release/p3_probe.wasm',
     'poolSize': 1, 'maxConcurrency': 2
   }]
 },
 'workload': {
   'allowedHosts': ['host.wasmcloud.internal:' + port],
   'allowedIpNameLookups': ['host.wasmcloud.internal'],
   'allowedHostLoopbackPorts': [int(port)],
   'environment': {'config': {
     'DATABASE_URL': 'postgres://postgres@host.wasmcloud.internal:' + port + '/postgres?sslmode=verify-full&sslrootcert=/certs/ca.crt'
   }}
 }
}
with open(out + '/wash/.wash/config.yaml', 'w') as f: json.dump(config, f, indent=2)
PY
wash -C "$out/wash" dev --non-interactive > "$out/wash.log" 2>&1 &
wash_pid=$!
ready=false
for _ in {1..60}; do
  kill -0 "$wash_pid" || { echo "wash failed; see $out/wash.log"; exit 1; }
  if curl -fsS --max-time 1 "http://127.0.0.1:$http_port/health" 2>/dev/null | grep -qx p3-ready; then ready=true; break; fi
  sleep 1
done
if [[ "$ready" != true ]]; then echo "wash startup timed out; see $out/wash.log"; exit 1; fi

request() {
  local name="$1" path="$2" expected="$3" code
  code="$(curl -sS --max-time 20 -o "$out/$name.txt" -w '%{http_code}' "http://127.0.0.1:$http_port/$path")"
  if [[ "$code" != "$expected" ]]; then
    echo "$name: expected HTTP $expected, got $code; see $out/$name.txt and $out/wash.log"
    exit 1
  fi
}
request tls tls 200
request untrusted-ca tls-bad-ca 500
grep -Ei 'UnknownIssuer|unknown issuer' "$out/untrusted-ca.txt" >/dev/null
echo 'PASS TLS verify-full: valid certificate accepted, untrusted CA rejected'

for i in 1 2 3; do request "reuse-$i" reuse 200; done
python3 - "$out" "$http_port" <<'PY'
import concurrent.futures, json, pathlib, sys, urllib.request
out, port = pathlib.Path(sys.argv[1]), sys.argv[2]
rows = [json.loads((out / f'reuse-{i}.txt').read_text()) for i in range(1, 4)]
assert len({r['instance'] for r in rows}) == 1, rows
assert len({r['pid'] for r in rows}) == 1, rows
assert [r['calls'] for r in rows] == [1, 2, 3], rows
assert all(r['tls'] for r in rows)
def get(_):
    with urllib.request.urlopen(f'http://127.0.0.1:{port}/reuse', timeout=10) as response:
        return json.load(response)
with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
    concurrent_rows = list(executor.map(get, range(2)))
assert all(r['instance'] == rows[0]['instance'] and r['pid'] == rows[0]['pid'] and r['tls'] for r in concurrent_rows), concurrent_rows
assert sorted(r['calls'] for r in concurrent_rows) == [4, 5], concurrent_rows
(out / 'reuse-summary.json').write_text(json.dumps(rows + concurrent_rows, indent=2))
PY
echo 'PASS warm instance: same Wasm state and PostgreSQL connection across sequential/concurrent HTTP requests'
request reuse-seaorm reuse-seaorm 200
python3 - "$out" <<'PY'
import json, pathlib, sys
out = pathlib.Path(sys.argv[1])
first = json.loads((out / 'reuse-1.txt').read_text())
orm = json.loads((out / 'reuse-seaorm.txt').read_text())
assert orm['instance'] == first['instance'] and orm['pid'] == first['pid'], (first, orm)
assert orm['calls'] == 6 and orm['tls'], orm
PY
echo 'PASS SeaORM entity API uses the same persistent TLS pool'
pid="$(python3 - "$out/reuse-1.txt" <<'PY'
import json, sys
print(int(json.load(open(sys.argv[1]))['pid']))
PY
)"
docker exec "$container" psql -U postgres -v ON_ERROR_STOP=1 -c "SELECT pg_terminate_backend($pid)" > "$out/terminate-backend.log"
request reuse-reconnected reuse 200
python3 - "$out" <<'PY'
import json, pathlib, sys
out = pathlib.Path(sys.argv[1])
first = json.loads((out / 'reuse-1.txt').read_text())
recovered = json.loads((out / 'reuse-reconnected.txt').read_text())
assert recovered['instance'] == first['instance'] and recovered['pid'] != first['pid'], (first, recovered)
assert recovered['calls'] == 7 and recovered['tls'], recovered
PY
echo 'PASS idle connection loss: same warm context, new verified TLS connection'
request cancellation cancel 200
python3 - "$out/cancellation.txt" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
assert r['server_cancel_sqlstate'] == '57014', r
assert all(v is True for k, v in r.items() if k != 'server_cancel_sqlstate'), r
PY
echo 'PASS cancellation: server cancellation/recovery, client deadline, explicit discard/reconnect'

install_cert wrong-name
request wrong-hostname tls 500
grep -Ei 'NotValidForName|not valid for name' "$out/wrong-hostname.txt" >/dev/null
echo 'PASS wrong certificate hostname rejected'
install_cert expired
request expired-cert tls 500
grep -Ei 'Expired|expired' "$out/expired-cert.txt" >/dev/null
echo 'PASS expired certificate rejected'
install_cert good
request restored-cert tls 200
docker exec "$container" psql -U postgres -c "ALTER SYSTEM SET ssl = 'off'" >/dev/null
docker exec "$container" psql -U postgres -c 'SELECT pg_reload_conf()' >/dev/null
sleep 1
request plaintext-downgrade tls 500
grep -Ei 'does not support TLS|does not support SSL' "$out/plaintext-downgrade.txt" >/dev/null
echo 'PASS plaintext fallback rejected'
request close-pool close-pool 200
pid="$(python3 - "$out/reuse-reconnected.txt" <<'PY'
import json, sys
print(int(json.load(open(sys.argv[1]))['pid']))
PY
)"
remaining="$(docker exec "$container" psql -U postgres -At -c "SELECT count(*) FROM pg_stat_activity WHERE pid = $pid")"
[[ "$remaining" == 0 ]] || { echo 'Pool close left its backend connection alive'; exit 1; }
echo 'PASS explicit pool close released its PostgreSQL backend'
echo "All results recorded under $out"
