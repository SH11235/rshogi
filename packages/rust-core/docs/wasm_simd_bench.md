# WASM SIMD ベンチ（fp32 行加算）

本ドキュメントは、`engine-core::simd::add_row_scaled_f32` を WebAssembly (wasm32) で計測する手順です。simd128 有効/無効の比較を行い、ブラウザ上での効果を確認します。

## 前提
- 最新ブラウザ（Chrome/Firefox/Safari/Edge）
- Rust 工具一式 + wasm-pack

## ビルド

- simd128 有効ビルド（推奨）
```bash
RUSTFLAGS="-C target-feature=+simd128" \
wasm-pack build crates/engine-wasm --release --target web
```

- フォールバック（simd128 無効）
```bash
RUSTFLAGS="-C target-feature=-simd128" \
wasm-pack build crates/engine-wasm --release --target web --out-dir pkg-nosimd
```

実運用では、`wasm-feature-detect`（npm）等でランタイム判定し、simd 対応なら simd 版を、非対応なら nosimd 版を読み込む 2 バンドル戦略が堅実です。

## 簡易ベンチ（ブラウザ）
以下の HTML を任意の静的サーバで配信して実行します（`python -m http.server` 等）。

```html
<!doctype html>
<html>
<meta charset="utf-8">
<body>
<script type="module">
  async function loadWasm(url) {
    const mod = await import(url);
    return mod;
  }
  // simd 対応の場合はこちらを優先
  const simdUrl = './pkg/engine_wasm.js'; // wasm-pack 出力
  // 非対応用（オプション）
  const nosimdUrl = './pkg-nosimd/engine_wasm.js';

  let mod;
  try {
    mod = await loadWasm(simdUrl);
  } catch (e) {
    console.warn('SIMD module failed, try nosimd', e);
    mod = await loadWasm(nosimdUrl);
  }

  const len = 256;
  const reps = 1_000_000; // 繰り返し回数（環境に応じ調整）
  const ks = [1.0, -1.0, 0.75];

  for (const k of ks) {
    const t0 = performance.now();
    const out = mod.bench_add_row_scaled(len, k, reps);
    const t1 = performance.now();
    console.log(`k=${k}: time=${(t1-t0).toFixed(1)}ms, out=${out}`);
  }
</script>
</body>
</html>
```

## 期待される傾向
- k=±1.0 は add/sub の専用経路が効くため、simd128 で明確に高速化（1.3〜2.0倍程度）
- k≠±1.0 は mul+add。simd128 で命令数削減の効果が見込める（1.2〜1.6倍目安）
- 環境（CPU/ブラウザ/電源管理）により差は変動します。`reps` を十分大きくし、ウォームアップ後に測定してください。

## 注意
- Wasm には FMA は無い（mul+add のみ）。本リポジトリの `nnue_fast_fma` は Wasm では意味を持ちません。
- JS↔Wasm の呼び出し回数が多いと性能が落ちます。ホットループは Wasm 内に閉じるように設計してください（本ベンチはその想定）。
- Threads/Atomics を併用した並列化は COOP/COEP/SharedArrayBuffer 要件が必要です。SIMD と併用すると更に向上余地がありますが、配布条件が厳しくなります。

## 実測（Node.js, wasm-pack --target nodejs）
- 条件: len=256, reps=500,000, 同一マシン（Node v22）, `engine-wasm` を simd 版/非 simd 版でビルド
- 測定スクリプト（概略）
  ```bash
  node -e "const simd=require('./crates/engine-wasm/pkg/engine_wasm.js'); \
           const nosimd=require('./crates/engine-wasm/pkg-nosimd/engine_wasm.js'); \
           function run(mod,name,reps){ const len=256; \
             for (const k of [1.0,-1.0,0.75]) { const t0=Date.now(); \
               const out=mod.bench_add_row_scaled(len,k,reps); const t1=Date.now(); \
               console.log(name,'k',k,'reps',reps,'ms',t1-t0,'out',out); } } \
           run(simd,'simd',500000); run(nosimd,'nosimd',500000);"
  ```
- 結果（ms, 小数点以下は省略）
  - simd:  k=1 → 14ms, k=-1 → 13ms, k=0.75 → 19ms
  - nosimd: k=1 → 81ms, k=-1 → 35ms, k=0.75 → 42ms
- 速度比（nosimd/simd の概算）
  - k=1: ≈ 5.8x, k=-1: ≈ 2.7x, k=0.75: ≈ 2.2x

ブラウザ（Web target）でも同等以上の傾向が期待できます。配布時は simd 版/非 simd 版の 2 バンドル戦略を推奨します。
