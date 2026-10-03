# P3 consumer integration tests

This is an independent Cargo workspace consuming published SQLx 0.9.0 and
SeaORM 2.0.4, with **only sqlx-core patched to this checkout**. The application
probes and fixtures originated in Oxlev's wasm-postgres-interop experiment.

## Requirements

- Rust 1.94 with wasm32-wasip1 (inherited from the fork's rust-toolchain.toml).
- wash with compatible P3 support; tested with the locally installed wash 2.10.0.
- Docker, Bash, Python 3, curl, OpenSSL and wasm-tools.
- A Wasm-capable clang; set CC_wasm32_wasip1 when it is not on PATH.
- sha256sum is not required here: this harness tests the checkout, not a downloaded patch.

```sh
cd tests/wasi-p3
rustup target add wasm32-wasip1
CC_wasm32_wasip1=clang ./scripts/p3.sh
CC_wasm32_wasip1=clang ./scripts/p3-hardening.sh
```

The scripts build and componentise the Rust guest, start PostgreSQL on a temporary
loopback-only port, launch wash, assert HTTP/database results and clean up both
processes. No cloud account, production credentials or main-project changes are used.

PostgreSQL defaults to postgres:16.14; POSTGRES_IMAGE overrides it. Test fixtures
use local trust authentication. TLS keys, CA certificates, logs and runtime
configuration are generated under ignored build/. Never commit that directory.
The HTTP ports default to 18081 and 18082; PROBE_HTTP_PORT overrides them.

## Coverage

- scripts/p3.sh: SQLx and SeaORM insert/read/NULL/rollback; pool acquisition
  timeout; concurrent queued queries; reuse of one physical connection.
- scripts/p3-hardening.sh: an explicitly warm HTTP sidecar retains its Rust pool
  across requests; TLS and certificate rejection cases; connection-loss recovery;
  cancellation semantics; database-side proof of pool closure.

The /instance probe in p3.sh records the primary component's default ephemeral
lifecycle. The hardening script uses an empty primary component so HTTP ingress
routes to a sidecar configured with poolSize=1 and maxConcurrency=2.

The scripts fail on unexpected results. Raw JSON assertions and diagnostic logs
remain in build/ for inspection. None of the probes are production endpoints.

For scope, implementation details and remaining gaps, see [WASI_P3.md](../../WASI_P3.md).
