#!/usr/bin/env python3
"""
指定した USI 対局ログ群から評価スパイクを検出し、
スパイク手の数手前（真の悪手候補）を複数バック量でターゲット化するツール。

出力は run_eval_targets.py が読む `targets.json` 形式（tag, pre_position）。

方針:
- 各ログで bestmove ごとの直前 eval(cp/mate) を拾い、隣接差分でスパイク判定。
- スパイク ply の直後に GUI が出す `position ...`（pos_after）を基準として、
  そこから back=2..5 手ぶん遡った `pre_position` を作る。
  （落下の“真犯人”が数手前にある前提で幅広くカバー）
- ログ間で重複する `pre_position` は除外（重複局面の多重評価を避ける）。

使い方例:
  scripts/analysis/make_targets_from_logs.py \
    --threshold 250 --topk 10 --out runs/diag-20251112-tuning \
    taikyoku-log/taikyoku_log_enhanced-parallel-202511*.md

出力:
  runs/diag-20251112-tuning/targets.json
  runs/diag-20251112-tuning/summary.txt
"""
import argparse
import json
import os
import re
from typing import List, Tuple, Optional


INFO_RE = re.compile(r"\binfo\b.*?score\s+(cp|mate)\s+(-?\d+)")
BESTMOVE_RE = re.compile(r"\bbestmove\s+([^\s]+)")
POS_LINE_RE = re.compile(r"\bposition\s+(startpos|sfen)\b.*")
POS_MOVES_SPLIT_RE = re.compile(r"\b moves \b")


def parse_bestmoves_with_positions(lines: List[str]) -> Tuple[List[dict], List[int]]:
    """bestmove ごとに pos_after と last_cp/last_depth を収集する。

    戻り値:
      - best: [ { 'idx':1-based, 'bestmove', 'pos_after', 'last_cp', 'last_depth' }, ... ]
      - evals: 各 bestmove 直前の評価値（cp 基準, mate は +-100000 に正規化）
    """
    best = []
    evals: List[int] = []
    cur_eval: Optional[int] = None

    # 直近の cp/depth をトラッキング
    last_cp: Optional[int] = None
    last_depth: int = -1

    for i, l in enumerate(lines):
        m = INFO_RE.search(l)
        if m:
            kind, val = m.group(1), int(m.group(2))
            if kind == "cp":
                cur_eval = val
                last_cp = val
            else:
                cur_eval = 100000 if val > 0 else -100000
                last_cp = cur_eval
            # depth（あれば）も拾う
            md = re.search(r"info\s+depth\s+(\d+)", l)
            if md:
                try:
                    last_depth = int(md.group(1))
                except ValueError:
                    pass
            continue

        bm = BESTMOVE_RE.search(l)
        if bm:
            # 次に現れる position 行を pos_after とする
            pos_after = None
            for j in range(i + 1, min(i + 80, len(lines))):
                if POS_LINE_RE.search(lines[j]):
                    pos_after = lines[j].split(" position ", 1)[1].strip()
                    break
            best.append(
                {
                    "idx": len(best) + 1,
                    "bestmove": bm.group(1),
                    "pos_after": pos_after,
                    "last_cp": last_cp,
                    "last_depth": last_depth,
                }
            )
            # 評価が未設定なら 0 を入れる（差分計算用の穴埋め）
            evals.append(cur_eval if cur_eval is not None else (evals[-1] if evals else 0))
            cur_eval = None
            continue

    return best, evals


def compute_spikes(evals: List[int], threshold: int) -> List[Tuple[int, int]]:
    spikes: List[Tuple[int, int]] = []
    prev = None
    for i, sc in enumerate(evals, start=1):
        if prev is None:
            prev = sc
            continue
        delta = sc - prev
        if abs(delta) >= threshold:
            spikes.append((i, delta))
        prev = sc
    return spikes


def chop_moves(pos_line: Optional[str], back_plies: int) -> Optional[str]:
    if not pos_line:
        return None
    if " moves " not in pos_line or back_plies <= 0:
        return pos_line
    # "startpos moves ..." も "sfen ... moves ..." も同一処理
    head, moves = POS_MOVES_SPLIT_RE.split(pos_line, maxsplit=1)[0], pos_line.split(" moves ", 1)[1]
    toks = [t for t in moves.strip().split() if t]
    if len(toks) >= back_plies:
        toks = toks[: -back_plies]
    else:
        toks = []
    return f"{head} moves {' '.join(toks)}" if toks else head


def main():
    ap = argparse.ArgumentParser(description="USIログからスパイク抽出し、数手遡りのターゲットを生成")
    ap.add_argument("logs", nargs="+", help="taikyoku_log_enhanced-parallel-*.md 等のログパス（複数可）")
    ap.add_argument("--threshold", type=int, default=250, help="abs(cp) スパイク閾値（既定: 250）")
    ap.add_argument("--topk", type=int, default=10, help="各ログの上位K件に制限（0=無制限, 既定:10）")
    ap.add_argument("--back-min", type=int, default=2, help="遡り最小手数（既定:2）")
    ap.add_argument("--back-max", type=int, default=5, help="遡り最大手数（既定:5）")
    ap.add_argument("--out", required=True, help="出力ディレクトリ（targets.json / summary.txt を出力）")
    args = ap.parse_args()

    os.makedirs(args.out, exist_ok=True)

    uniq_positions = set()
    targets = []
    summary_lines = []

    for path in args.logs:
        try:
            with open(path, "r", encoding="utf-8", errors="ignore") as f:
                lines = [ln.rstrip("\n") for ln in f]
        except FileNotFoundError:
            summary_lines.append(f"SKIP(not found): {path}")
            continue

        best, evals = parse_bestmoves_with_positions(lines)
        if not best:
            summary_lines.append(f"SKIP(no bestmove): {os.path.basename(path)}")
            continue

        spikes = compute_spikes(evals, args.threshold)
        if args.topk and len(spikes) > args.topk:
            spikes = sorted(spikes, key=lambda x: abs(x[1]), reverse=True)[: args.topk]

        base = os.path.basename(path)
        summary_lines.append(
            f"{base}: plies={len(best)} spikes={len(spikes)} (threshold={args.threshold})"
        )

        for (ply, delta) in spikes:
            if ply < 1 or ply > len(best):
                continue
            b = best[ply - 1]
            for k in range(args.back_min, args.back_max + 1):
                pos = chop_moves(b.get("pos_after"), k)
                if not pos:
                    continue
                if pos in uniq_positions:
                    continue
                uniq_positions.add(pos)
                tag = f"{os.path.splitext(base)[0]}_ply{ply}_back{k}"
                targets.append(
                    {
                        "tag": tag,
                        "pre_position": pos,
                        "origin_log": base,
                        "origin_ply": ply,
                        "origin_delta": delta,
                        "back_plies": k,
                    }
                )

    # 書き出し
    out_json = os.path.join(args.out, "targets.json")
    with open(out_json, "w", encoding="utf-8") as wf:
        json.dump({"targets": targets}, wf, ensure_ascii=False, indent=2)

    with open(os.path.join(args.out, "summary.txt"), "w", encoding="utf-8") as wf:
        wf.write("\n".join(summary_lines) + "\n")
        wf.write(f"unique_targets={len(targets)}\n")

    print(f"wrote targets={len(targets)} -> {out_json}")


if __name__ == "__main__":
    main()

