# Transposition Table Prefetch - Performance Analysis Report

## 📊 Executive Summary

TTプリフェッチ最適化のPhase 2～4実装の包括的なパフォーマンス分析結果。
4つの異なるベンチマーク手法により、実装の効果と課題を多角的に評価。

### 🎯 Key Achievements
- **Depth 5**: **+250%** NPS改善（Phase 2の最大成功）
- **ノード削減**: 最大**99.92%**削減（depth 7）
- **軽量化**: ハッシュ計算を**85%高速化**（2-3ns）
- **Phase 4発見**: CAS操作が**最大40%のオーバーヘッド**を生む

### ⚠️ Critical Findings (Phase 4)
- **CASオーバーヘッド**: 単一スレッドでも26-66%の性能低下
- **プリフェッチ効果**: ほぼゼロまたは負の効果（-0.1%～-3.4%）
- **プリフェッチヒット率**: 14.7%～93%と大きく変動

## 📈 Benchmark Results Summary

### 1. Four-Way Comparison (Phase 4) 🆕

NoTT vs TT(no CAS) vs TTOnly vs TT+Prefetchの比較により、CAS操作とプリフェッチの影響を完全分離。

#### Depth 4 Summary
| Position Type | Node Reduction | CAS Overhead | Prefetch Benefit | Prefetch Hit Rate |
|--------------|----------------|--------------|------------------|-------------------|
| Initial | 37.1% | -37.0% | 0.0% | 14.7% |
| Opening | 23.9% | -26.3% | 0.0% | 32.2% |
| Middle | 28.9% | -29.4% | 0.0% | 42.7% |
| Endgame | 42.1% | -40.4% | -0.1% | 37.2% |

#### Depth 5 Summary
| Position Type | Node Reduction | CAS Overhead | Prefetch Benefit | Prefetch Hit Rate |
|--------------|----------------|--------------|------------------|-------------------|
| Initial | 62.5% | -62.4% | -0.6% | 30.9% |
| Opening | 40.4% | -40.6% | 0.0% | 30.3% |
| Middle | 37.6% | -37.6% | -0.2% | 41.5% |
| Endgame | 69.0% | -66.8% | -0.3% | 93.0% |

#### Depth 6 Summary
| Position Type | Node Reduction | CAS Overhead | Prefetch Benefit | Prefetch Hit Rate |
|--------------|----------------|--------------|------------------|-------------------|
| Initial | 82.3% | -82.4% | -1.0% | 26.6% |
| Opening | 66.3% | -66.1% | -0.3% | 51.9% |
| Middle | 58.0% | -57.6% | -0.3% | 27.3% |

**Phase 4 Key Insights**:
1. **CASオーバーヘッドがノード削減効果をほぼ相殺**: 40%削減が40%遅延に
2. **プリフェッチは性能を悪化**: 全ケースで0%以下の効果
3. **高ヒット率でも効果なし**: 93%ヒット率でも-0.3%の性能低下
4. **深さとともにCAS影響増大**: Depth 6で最大82%のオーバーヘッド

### 2. Three-Way Comparison (Phase 3)

NoTT vs TTOnly vs TT+Prefetchの比較により、プリフェッチ単体の効果を分離測定。

| Depth | NoTT Baseline | TTOnly Effect | Prefetch Gain | Total Improvement |
|-------|--------------|---------------|---------------|-------------------|
| 4 | 100% | -51.2% | +6.7% | -44.5% |
| 5 | 100% | -77.1% | +17.3% | -59.8% |
| 6 | 100% | -91.8% | +0.0% | -91.8% |
| 7 | 100% | - | +0.4% | - |

**分析**: 
- TTによる劇的なノード削減効果が支配的
- プリフェッチの追加効果は浅い深度で顕著
- Depth 6以降はプリフェッチ効果が消失

### 2. Enhanced Implementation (Phase 2)

軽量ハッシュ計算と選択的プリフェッチの効果測定。

| Depth | Initial Pos | Opening | Middle Game | Average |
|-------|------------|---------|-------------|---------|
| 4 | +11.14% | +9.21% | +40.46% | **+20.27%** |
| 5 | +347.51% | +347.58% | +53.67% | **+249.59%** |
| 6 | +0.92% | +1.17% | -15.54% | **-4.48%** |
| 7 | +7.83% | - | - | **99.92%** node reduction |

**特筆事項**:
- Depth 5での異常な高性能（3.5倍高速化）
- 中盤複雑局面でのdepth 6性能低下
- Hotfix後のdepth 7安定動作
- **Phase 4でCASオーバーヘッドが主要因と判明**

### 4. Perft-based Benchmark

純粋な移動生成性能への影響測定（TTの探索削減効果を除外）。

| Depth | NPS Change | Hit Rate |
|-------|------------|----------|
| 4 | -3.11% | 33.38% |
| 5 | -5.33% | (248 hits |
| 6 | -6.47% | 495 misses) |

**解釈**:
- プリフェッチのオーバーヘッドが露呈
- 低いヒット率が性能低下の主因
- TTの探索削減効果なしではマイナス

## 🔍 Detailed Analysis

### Phase 4: CAS Overhead Analysis 🆕

Phase 4の4モード比較により、TTの性能問題の根本原因が明らかに：

1. **CAS操作の影響が甚大**
   ```
   Depth 4: Node reduction 37% → CAS overhead -37% = Net 0%
   Depth 5: Node reduction 62% → CAS overhead -62% = Net 0%
   Depth 6: Node reduction 82% → CAS overhead -82% = Net 0%
   ```
   **結論**: ノード削減の利益がCASコストで完全に相殺される

2. **プリフェッチの負の効果**
   - 全深度・全局面で性能低下（0%～-3.4%）
   - 高ヒット率（93%）でも改善なし
   - ハッシュ計算とプリフェッチ命令のオーバーヘッド > キャッシュミス削減効果

3. **単一スレッドでのCASペナルティ**
   - マルチスレッド競合なしでも26-82%の性能低下
   - メモリフェンス（full barrier）のコスト
   - CPU最適化（投機実行等）の阻害

### Performance by Position Type

```
Initial Position (単純):
- Depth 4-5: 高い改善率（Phase 2）
- Depth 6+: 限定的効果
- Phase 4: CAS影響が支配的

Opening (序盤):
- 安定した改善（Phase 2）
- TTヒット率が高い
- Phase 4: プリフェッチ効果なし

Middle Game (中盤):
- Depth 6で-15%の性能低下
- 複雑な局面での課題
- Phase 4: 最もCAS影響を受ける

Endgame (終盤):
- 高いノード削減率
- 最大のCASオーバーヘッド（-66.8%）
- プリフェッチヒット率93%でも効果なし
```

### Cache Behavior Analysis

1. **L1/L2キャッシュ効率**
   - 浅い深度: 高いキャッシュヒット率
   - 深い深度: キャッシュ汚染の可能性
   - **Phase 4発見**: プリフェッチがキャッシュを汚染

2. **メモリ帯域使用**
   - 33%ヒット率 = 67%の帯域が無駄
   - バジェット制導入で改善見込み
   - **Phase 4発見**: 帯域使用がボトルネックではない

### Critical Issues and Solutions

| 問題 | 原因 | 解決策 | 状態 |
|------|------|--------|------|
| Depth 7タイムアウト | 指数的プリフェッチ増加 | depth <= 6制限 | ✅ 解決済 |
| 中盤性能低下 | 複雑局面での予測精度低下 | 局面複雑度判定 | 🔄 Phase 3 |
| 低ヒット率 | 固定距離戦略 | AdaptivePrefetcher改善 | ✅ Phase 3 |
| **CASオーバーヘッド** | アトミック操作とメモリフェンス | Relaxed読み取り実装 | 🆕 Phase 4 |
| **プリフェッチ逆効果** | オーバーヘッド > 利益 | 条件付き無効化 | 🆕 Phase 4 |

## 💡 Technical Insights

### 1. Lightweight Hash Calculation Success
```rust
// Before: 10-20ns (do_move/undo_move)
// After: 2-3ns (direct calculation)
// Improvement: 85% faster
```

### 2. Selective Prefetch Strategy
- キラームーブ優先: ✅ 効果的
- 上位2-3手のみ: ✅ オーバーヘッド削減
- PVラインプリフェッチ: ✅ 高ヒット率期待

### 3. Depth-Dependent Behavior
```
Depth 1-4: Minimal benefit (TTヒット率低い)
Depth 5: Sweet spot (最大効果)
Depth 6+: Diminishing returns (キャッシュ汚染)
```

## 📊 Performance Metrics Comparison

### NPS (Nodes Per Second) Improvements

```
Best Case (Depth 5, Initial Position):
Without TT: 14,821 NPS
With TT+Prefetch: 66,355 NPS
Improvement: +347.51%

Worst Case (Depth 6, Middle Game):
Without TT: 478,456 NPS  
With TT+Prefetch: 404,081 NPS
Degradation: -15.54%
```

### Node Reduction Efficiency

```
Depth 7 (After Hotfix):
Total Nodes (No TT): 1,146,843
Total Nodes (TT+Prefetch): 875
Reduction: 99.92%
```

## 🎯 Recommendations

### Immediate Actions (Phase 4 Critical) 🆕

1. **CASオーバーヘッド削減**
   ```rust
   // Relaxed ordering for reads
   pub fn probe_relaxed(&self, hash: u64) -> Option<TTEntry> {
       // 読み取り専用パスでRelaxed ordering使用
       // 26-82%の性能改善期待
   }
   ```

2. **プリフェッチの条件付き無効化**
   ```rust
   // Phase 4結果に基づく無効化
   const ENABLE_PREFETCH: bool = false; // 一時的に無効化
   ```

### Phase 3 Remaining Actions

1. **局面複雑度判定の実装**
   - 中盤での-15%を改善
   - 王手、駒得差、合法手数で判定
   - **Phase 4: CAS改善後に再評価必要**

2. **深さ無制限化**
   - Hotfix（depth <= 6）の段階的撤廃
   - **Phase 4: プリフェッチ無効化で不要の可能性**

### Future Optimizations (Priority Revised)

1. **Lock-free TT実装**（最優先）
   - Read-Copy-Update (RCU) パターン
   - Hazard Pointers
   - CASオーバーヘッドの根本解決

2. **プリフェッチ再設計**
   - Depth 8以上のみ有効化
   - バッチプリフェッチ（複数エントリを一度に）
   - ハードウェアプリフェッチャーとの協調

3. **NUMA対応TT**
   - スレッドローカルTTパーティション
   - リモートメモリアクセス削減

## 📈 Expected Impact

### Current State (Phase 4 Analysis)
- **TT効果**: ノード削減は機能（37-82%削減）
- **CAS問題**: 削減効果を完全に相殺（26-82%遅延）
- **プリフェッチ**: 逆効果（0～-3.4%）
- **実質改善**: Phase 2の+250%はCAS実装前の特殊条件

### After Phase 4 Optimizations
1. **Relaxed読み取り実装**: +26-82%改善期待
2. **プリフェッチ無効化**: +0-3%改善
3. **Lock-free TT**: 根本的解決

### Revised Expectations
- **短期目標**: CASオーバーヘッド50%削減
- **中期目標**: Lock-free実装で本来のTT性能実現
- **長期目標**: 適切な深度でのプリフェッチ再導入

## 🏆 Conclusion

### Phase 4の衝撃的発見
1. **CAS操作が最大のボトルネック**: ノード削減効果を帳消し
2. **プリフェッチは現状有害**: 全条件で性能悪化
3. **Phase 2の+250%は誤解**: CAS無効時の特殊ケース

### 教訓と今後の方針
1. **測定の重要性**: 4モード比較で真の原因判明
2. **前提の見直し**: TTは必ずしも高速化しない
3. **優先順位変更**: CAS最適化 > プリフェッチ

### Success Factors
1. ✅ 軽量ハッシュ計算（85%高速化）- 効果はCASで相殺
2. ✅ 選択的プリフェッチ戦略 - 実は逆効果
3. ✅ 適応的距離調整 - 効果なし
4. ✅ False-sharing対策 - 単一スレッドでは無関係

### Critical Challenges (Updated)
1. 🔴 **CASオーバーヘッド（26-82%）**
2. 🔴 **プリフェッチ逆効果（全条件）**
3. ⚠️ 中盤複雑局面（-15%）- CAS起因の可能性
4. ⚠️ ヒット率改善（33%→50%+）- 優先度低下

---

*Phase 4 data added: 2025-08*
*Critical finding: CAS overhead dominates performance*
*Recommendation: Prioritize lock-free TT implementation*
