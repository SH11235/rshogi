#!/usr/bin/env bash
set -euo pipefail

OUT_DIR="docs/performance/data"
DO_MACRO_NATIVE=false
DO_MACRO_GENERIC=false
DO_WASM_NODE=false
DO_MICRO=false

usage() {
  cat <<USAGE
Usage: $0 [--out DIR] [--macro-native] [--macro-generic] [--wasm-node] [--micro]
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUT_DIR="$2"; shift 2;;
    --macro-native) DO_MACRO_NATIVE=true; shift;;
    --macro-generic) DO_MACRO_GENERIC=true; shift;;
    --wasm-node) DO_WASM_NODE=true; shift;;
    --micro) DO_MICRO=true; shift;;
    -h|--help) usage; exit 0;;
    *) echo "Unknown arg: $1"; usage; exit 1;;
  esac
done

mkdir -p "$OUT_DIR"

run_engine_usi_once() {
  local out_file="$1"
  local rustflags="$2"
  local features="$3" # e.g. "--features fast-fma" or empty
  RUSTFLAGS="$rustflags" cargo build -p engine-usi --release -q $features
  (
    {
      printf 'usi\n'
      printf 'isready\n'
      printf 'setoption name Threads value 1\n'
      printf 'setoption name EngineType value EnhancedNnue\n'
      printf 'setoption name EvalFile value runs/nnue_local/nn_best.fp32.bin\n'
      printf 'isready\n'
      printf 'position startpos\n'
      printf 'go movetime 3000\n'
      sleep 5
      printf 'quit\n'
    } | target/release/engine-usi
  ) > "$out_file" 2>&1 || true
}

avg_nps_from_runs() {
  awk '/^info depth/{n=gensub(/.* nps ([0-9]+).*/,"\\1",1); last=n} END{print (last?last:0)}' "$1"
}

collect_macro() {
  local rustflags="$1"; local label="$2"; local features="$3"
  local tmp
  local sum=0
  local count=0
  for i in 1 2 3; do
    tmp=$(mktemp)
    run_engine_usi_once "$tmp" "$rustflags" "$features"
    nps=$(avg_nps_from_runs "$tmp")
    sum=$((sum + nps)); count=$((count + 1))
    rm -f "$tmp"
  done
  if (( count > 0 )); then
    echo "$label,$((sum / count))"
  fi
}

if $DO_MACRO_NATIVE; then
  {
    echo "mode,avg_nps"
    collect_macro "-C target-cpu=native" native_base ""
    collect_macro "-C target-cpu=native" native_fma "--features fast-fma"
  } > "$OUT_DIR/engine_usi_native.csv"
  echo "[OK] Wrote $OUT_DIR/engine_usi_native.csv"
fi

if $DO_MACRO_GENERIC; then
  {
    echo "mode,avg_nps"
    collect_macro "-C target-feature=-avx,-avx2,-fma -C target-cpu=x86-64" generic_base ""
    collect_macro "-C target-feature=-avx,-avx2,-fma -C target-cpu=x86-64" generic_fma "--features fast-fma"
  } > "$OUT_DIR/engine_usi_generic.csv"
  echo "[OK] Wrote $OUT_DIR/engine_usi_generic.csv"
fi

if $DO_WASM_NODE; then
  if command -v node >/dev/null 2>&1; then
    RUSTFLAGS="-C target-feature=+simd128 --cfg=getrandom_backend=\"wasm_js\"" wasm-pack build crates/engine-wasm --release --target nodejs >/dev/null 2>&1 || true
    RUSTFLAGS="-C target-feature=-simd128 --cfg=getrandom_backend=\"wasm_js\"" wasm-pack build crates/engine-wasm --release --target nodejs --out-dir pkg-nosimd >/dev/null 2>&1 || true
    {
      echo "build,k,ms"
      node -e "const simd=require('./crates/engine-wasm/pkg/engine_wasm.js'); const nosimd=require('./crates/engine-wasm/pkg-nosimd/engine_wasm.js'); function run(mod,name){ const len=256; const reps=500000; for (const k of [1.0,-1.0,0.75]) { const t0=Date.now(); const out=mod.bench_add_row_scaled(len,k,reps); const t1=Date.now(); console.log(name+','+k+','+(t1-t0)); } } run(simd,'simd'); run(nosimd,'nosimd');" 2>/dev/null
    } > "$OUT_DIR/wasm_node.csv"
    echo "[OK] Wrote $OUT_DIR/wasm_node.csv"
  else
    echo "[SKIP] Node.js not found; skip wasm-node"
  fi
fi

if $DO_MICRO; then
  log=$(mktemp)
  RUSTFLAGS="-C target-cpu=native" cargo bench -p engine-core --bench nnue_add_row_bench >"$log" 2>&1 || true
  {
    echo "size,k,dispatcher_ns,scalar_ns"
    for size in 256 2048; do
      for k in 1.0 -1.0 0.75; do
        d=$(awk "/nnue_add_row_f32\/dispatcher\/len=${size},k=${k}/{flag=1;next} flag&&/time:/{print;flag=0}" "$log" | sed -n 's/.*\[.* ns \([0-9.]*\) ns .*/\1/p')
        s=$(awk "/nnue_add_row_f32\/scalar\/len=${size},k=${k}/{flag=1;next} flag&&/time:/{print;flag=0}" "$log" | sed -n 's/.*\[.* ns \([0-9.]*\) ns .*/\1/p')
        if [[ -n "$d" && -n "$s" ]]; then
          echo "$size,$k,$d,$s"
        fi
      done
    done
  } > "$OUT_DIR/micro_native.csv"
  rm -f "$log"
  echo "[OK] Wrote $OUT_DIR/micro_native.csv"
fi

echo "Done."

