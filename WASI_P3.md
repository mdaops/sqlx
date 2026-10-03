# Experimental WASI Preview 3 runtime

This mdaops fork adds an **opt-in PostgreSQL/WASI P3 runtime backend to sqlx-core**.
It is not an upstream-supported SQLx feature or a wasmCloud host plugin.

Base: SQLx 0.9.0 published source revision
`003b698e99e024f3621b8043a2426fde5b741171`, as recorded in the published
sqlx-core crate's .cargo_vcs_info.json. The fork branch is
`feat/wasi-p3-runtime`; consumers should pin an exact tested commit.

## Using it

Keep sqlx and SeaORM from crates.io; override only sqlx-core in your workspace:

```toml
[dependencies]
sqlx = { version = "=0.9.0", default-features = false, features = [
    "postgres", "derive", "tls-rustls-ring-webpki",
] }

[target.'cfg(target_os = "wasi")'.dependencies]
# Experimental internal feature; not part of upstream's stable API.
sqlx-core = { version = "=0.9.0", features = ["_rt-wasi-p3"] }

[patch.crates-io]
sqlx-core = { git = "https://github.com/mdaops/sqlx", rev = "<tested-commit>" }
```

Replace the placeholder with the full commit you have tested and commit Cargo.lock.
Do not enable Tokio networking or another native runtime in the WASI dependency
graph. Native binaries can enable their runtime through target-specific dependencies.

The application must execute inside a **P3 async entry point**, not a P1 main
function or a synchronous block_on wrapper. Rust's wasm32-wasip1 compiler target
plus componentisation is the tested build route; the imported interfaces are P3.

Use a Wasm-capable clang for ring, e.g. `CC_wasm32_wasip1=clang`.
The host must implement compatible P3 sockets, clocks and task execution and grant
the required network/name-lookup permissions. This does not bypass host policy.

## What is implemented

- P3 TCP streams and asynchronous name lookup.
- Monotonic timers, timeouts and local task spawning.
- Runtime-independent pool semaphore support.
- TLS through SQLx's existing rustls/ring implementation, including the transport
  flushing needed for buffered P3 writes.
- Normal SQLx query/row/transaction APIs and SeaORM's SQLx PostgreSQL backend.

All production code changes are in sqlx-core. Native runtime dispatch remains
unchanged when the feature is disabled. No changes to sqlx-postgres or SeaORM
are required.

## Live evidence

The reproducible consumer harness is in [tests/wasi-p3](tests/wasi-p3/README.md).
It runs the actual component under wash against a disposable local PostgreSQL.

Verified:
- Typed insert/read, bound parameters, NULLs and rollback through SQLx and SeaORM.
- Pool acquisition timeout and queued queries.
- Same Wasm state and same PostgreSQL TLS connection across sequential and
  concurrent HTTP requests in an explicitly warm component.
- SeaORM sharing that retained pool.
- Recovery after an idle pooled backend is terminated; explicit pool closure.
- TLS verify-full: trusted certificates accepted; unknown CA, wrong hostname,
  expiry and plaintext downgrade rejected.
- Explicit server cancellation yields SQLSTATE 57014; rollback restores the same
  connection.
- A client timeout abandons the future but does not cancel server work. The test
  explicitly cancels server work, discards the connection and reconnects.

## Important limits

This remains an experimental fork, not a production-readiness claim.

- **Timeout is not cancellation.** The proof uses pg_cancel_backend from an
  observer connection; it does not add a PostgreSQL wire CancelRequest API.
  Choose a deliberate statement_timeout/cancellation/disposal policy.
- Pools live per Wasm instance. Default wash dev primary components are ephemeral;
  the harness explicitly enables warm sidecar instances. Saturation, retirement
  and redeployment can discard state.
- SCRAM/password authentication, mTLS, heavy-load cancellation, large-payload
  stress and instance retirement are not covered by the live suite.
- Blocking worker pools and Unix-domain sockets are not implemented for P3.
  Filesystem helpers that require spawn_blocking are not supported. The TLS probe
  reads its mounted CA with std::fs and uses ssl_root_cert_from_pem instead.
- Compile-time query! checking, migrations, SQLite and MySQL are not validated.
- wasip3 0.9.0 and wit-bindgen 0.62.0 are deliberately pinned.

No upstream PR or compatibility promise is implied. The branch is intended to
support further integration experiments while retaining ordinary database APIs.
