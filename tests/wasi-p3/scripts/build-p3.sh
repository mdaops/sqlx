#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p build
# ring contains C code. A native GCC selected by a parent environment silently
# produces the wrong object format; select a Wasm-capable compiler explicitly.
export CC_wasm32_wasip1="${CC_wasm32_wasip1:-clang}"
command -v "$CC_wasm32_wasip1" >/dev/null || {
  echo 'Set CC_wasm32_wasip1 to a Wasm-capable clang executable.' >&2
  exit 1
}
cargo build --locked -p p3-probe --target wasm32-wasip1 --release > build/p3-build.log 2>&1
mkdir -p build/componentise/.wash
python3 - "$PWD" <<'PY'
import json, sys
root = sys.argv[1]
with open(root + '/build/componentise/.wash/config.yaml', 'w') as f:
    json.dump({'build': {'command': 'true', 'component_path': root + '/target/wasm32-wasip1/release/p3_probe.wasm'}}, f)
PY
wash -C "$PWD/build/componentise" build > build/p3-componentise.log 2>&1
