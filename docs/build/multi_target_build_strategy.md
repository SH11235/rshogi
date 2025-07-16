# WASM / ネイティブ向けビルド戦略 — マルチターゲット対応

**対象 UI**  
- デスクトップ (Tauri)  
- Web (GitHub Pages / Cloudflare Pages)  
- Discord ボット (CLI / サーバ)  
- モバイルアプリ (ネイティブ or PWA)

---

## 1. プラットフォーム別に使える最適化

| プラットフォーム | SIMD (`simd128`) | Wasm Threads<br>`atomics` + `SharedArrayBuffer` | Lazy SMP<br>(マルチコア探索) | メモ |
|------------------|-----------------|-----------------------------------------------|------------------------------|------|
| **Tauri デスクトップ** | ✅ 常時 | ネイティブ呼び出し推奨 | ✅ (Rayon など) | Rust バックエンドを直接呼び出す |
| **Discord ボット / サーバ** | ✅ | n/a | ✅ | CLI/ライブラリとしてビルド |
| **Cloudflare Pages** | ✅ | ✅ *※COOP+COEP 必要* | ✅ | `_headers` で COOP/COEP を付与 |
| **GitHub Pages** | ✅ | 🚫 (SAB ブロック) | 🚫 | **シングルスレッド**版のみ配置 |
| **モバイルアプリ (ネイティブ)** | ✅ (+NEON) | n/a | ✅ | FFI で呼び出し |
| **モバイル PWA / ブラウザ** | ✅ | 端末・ヘッダー次第 | ❔ | iOS WKWebView は SAB 不可 |

---

## 2. Cargo ビルド例

```bash
# ネイティブ (SIMD + 並列)
cargo build --release --features "simd parallel"

# Cloudflare Pages (SIMD + Threads)
RUSTFLAGS="-C target-feature=+simd128,+atomics" cargo build --target wasm32-unknown-unknown --release --features "simd wasm_threads"

# GitHub Pages (SIMD のみ)
RUSTFLAGS="-C target-feature=+simd128" cargo build --target wasm32-unknown-unknown --release --features "simd"
```

### Cargo features のサンプル

```toml
[features]
default = ["simd"]
simd     = []          # SIMD 命令を有効化
parallel = ["rayon"]   # ネイティブ Lazy SMP
wasm_threads = []      # Wasm Threads (+atomics)
```

---

## 3. JS ローダーでビルドを切替

```js
export async function initEngine() {
  const mtOk = self.crossOriginIsolated &&
               typeof SharedArrayBuffer !== 'undefined';

  const wasmUrl = mtOk ? 'engine_mt.wasm' : 'engine_st.wasm';
  const { init } = await import(`./${wasmUrl}`);

  return init({
    threads: mtOk ? navigator.hardwareConcurrency : 1
  });
}
```

---

## 4. Cloudflare Pages 用ヘッダー

プロジェクト直下に **`_headers`** ファイルを置く:

```
/*
  Cross-Origin-Opener-Policy: same-origin
  Cross-Origin-Embedder-Policy: require-corp
```

GitHub Pages はカスタムヘッダーが使えないため、`engine_st.wasm` のみをホストします。

---

## 5. 実装優先度

1. **SIMD** … すべてのターゲットで効果があり導入も簡単  
2. **Lazy SMP (ネイティブ)** … デスクトップ / サーバ / ボットで Elo 向上  
3. **Wasm Threads** … Cloudflare Pages や PWA で COOP/COEP を張れる場合に解禁  
4. **ST フォールバック** … SAB 不可環境（GitHub Pages / iOS WKWebView）用

---

## 6. まとめ

- **ビルドを 2 系統 (MT / ST)** 用意し、ランタイムで自動判定  
- **ヘッダーを設定できるホスト** では MT 版を配信  
- **設定できない場合** は ST 版を安全にロード  
- SIMD はビルド共通で常に有効

これで **最大性能を確保しつつ、どの環境でもクラッシュせず動く** 構成が実現できます。
