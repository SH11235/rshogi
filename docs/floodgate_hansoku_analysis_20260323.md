# Floodgate 反則負け分析 (2026-03-22〜23)

## サマリ

- **対局数**: 17局（RAMU_TP: 1勝16敗）
- **反則負け**: 10局（全敗因の62.5%）
- **逆転負け**: 4局
- **時間切れ**: 1局
- **順当な負け**: 1局
- **勝ち**: 1局

## 反則負けの特徴

- **10局中6局が勝勢（Mate読み中）で反則負け**
- 全てKIFのPVコメントから推定すると「駒打ち」手
- 対 komadokun_depth8 は4戦全敗、全て +Mate で反則負け
- エンジン単体テストでは全10局面で合法手を返す（再現不可）

## 調査結果

### エンジン側（問題なし）

1. `root_moves` は `generate_legal()` → `is_legal()` で検証済みの合法手のみ
2. `bestmove` 出力は `root_moves` から選択（合法性保証）
3. `Move::to_usi()` の座標変換は正しい
4. stdout のフラッシュは明示的に実施
5. ponder 中の時間管理は USI 仕様準拠（`is_pondering` 時は停止判定をスキップ）
6. 全10局面を再現テストし、全て合法な bestmove を返すことを確認

### 接続層（Shogidokoro）

- Shogidokoro (Windows) 経由で floodgate に接続
- 広く使われているソフトのため単体のバグは考えにくい
- ただし USI プロトコルの微妙な動作差異（ponder 時の bestmove タイミング等）が問題を引き起こす可能性あり

### 未解明の可能性

1. **ponder miss 時の bestmove 二重出力**: `cmd_go` 内の `cmd_stop()` が前の ponder 探索を停止→bestmove 出力。Shogidokoro がこの bestmove を誤って使用する可能性
2. **位置同期ずれ**: Shogidokoro が送る position と floodgate の実際の局面がずれる
3. **Windows ↔ Linux 間の通信問題**: SSH/リモート実行時のバッファリング・遅延

## 試行と却下

### bestmove 合法性チェック＋差し替え（却下・revert済み）

bestmove 出力前に `generate_legal()` で検証し、不正ならログ記録＋合法手先頭で代替する防御策を `main.rs` に実装したが、
**根本原因を隠蔽する**ため却下し revert した。

参考コード断片（`cmd_go` の search thread 内、bestmove 出力前）:
```rust
// bestmove の合法性チェック
let mut legal_moves = MoveList::new();
generate_legal(&pos, &mut legal_moves);
let is_legal = legal_moves.as_slice().contains(&result.best_move);
if !is_legal {
    eprintln!(
        "info string CRITICAL: bestmove {} is ILLEGAL in position {}",
        result.best_move.to_usi(), pos.to_sfen(),
    );
    // ログファイルにも記録
    // ... rshogi_bestmove_errors.log に書き出し ...
    // 合法手リストの先頭で代替 ← これがバグ隠蔽になる
}
```

USI 通信ログ（全 bestmove を `rshogi_usi.log` に SFEN 付きで記録）も同時に実装したが、まとめて revert。

**教訓**: 原因が不明な段階で防御的にすり替えるとデバッグが困難になる。
ログのみ（差し替えなし）であれば再検討の余地あり。

## 次のステップ

1. **Shogidokoro 側のログを確認**: Shogidokoro にはエンジン通信ログ機能がある。次の対局で有効化し、
   エンジンが実際に送った bestmove と floodgate に送信された手を照合する
2. **反則負けが再発した場合**:
   - エンジンの bestmove が合法 → Shogidokoro/通信層の問題
   - エンジンの bestmove が不正 → エンジンバグ（探索/TT/ponder 関連）
3. **CSA プロトコル直接実装** を検討（Shogidokoro を介さずにバイパスし、変数を減らす）

## 反則負け局面 SFEN

```
# Game 1: vs PC1_save012 (先手, -35281)
position sfen 9/4g1g2/3skp2l/2ppp1ppP/3n1P3/2PG1SP2/2+bPP1N2/P3sR3/2K4RL b B2L2Pgs2n3p 109

# Game 2: vs pt-v0.0.2 (後手, +Mate:5)
position sfen ln1g5/9/pppp2k2/3s2p2/7+rl/2P6/PP1PGg3/9/+b+b2K4 w RLPg3s3nl8p 100

# Game 3: vs PC1_save012 (後手, -Mate:10)
position sfen lns4pp/9/p1pp1+BG1k/4ppp2/4N3L/P4P3/+b2PP1P1P/1+rS2KS2/+p2G1G1NL w NL3Prgs 84

# Game 4: vs komadokun (先手, +Mate:5)
position sfen lns1k4/4r2G1/p1p1+R4/1p3p2p/9/2P3P2/1+bSPPP2P/3GK2S1/5G1NL b BGSN6Pn2lp 69

# Game 5: vs Yss1000k (先手, -Mate:14)
position sfen l2g3nl/1k7/pps2BGpp/2sp2p2/Pn5P1/1K4P2/1P1PpP2P/6S2/L1+r3BNL b GN2Prgs2p 89

# Game 6: vs komadokun (先手, +Mate:5)
position sfen 1ns2G1n1/l3k4/1pppp1pp1/p8/6KP1/2P1l1n2/PP1P1PP2/1B3S3/LNSG5 b RGS3Prbglp 59

# Game 7: vs pt-v0.0.2 (後手, +Mate:7)
position sfen ln7/1s1rk4/p2bp4/2p4+bp/1s1p1+r3/P1g6/4PsPP1/3+l3K1/g6NL w 2gs2nl9p 118

# Game 8: vs komadokun (先手, +Mate:9)
position sfen lnsg1k2l/6r2/p1pp+B3p/1p2p1+R2/4b2p1/2P6/PP1P1PP1P/4K4/LNSG1GSNL b GS3Pnp 55

# Game 9: vs PC1_save012 (後手, -Mate:12)
position sfen lns2k1+P1/1r7/pppp+R3p/5pl2/3b2p1S/2P2P2P/PPSPP1P2/3KG2+s1/LN1G4L w 2GN2Pbn 80

# Game 10: vs komadokun (先手, +Mate:5)
position sfen lnk5l/r2s1+B1p1/pgp+RN3p/1p4p2/6n2/2P6/PP1PPPP1P/1K7/3G1GSNL b BGSL4Ps 53
```
