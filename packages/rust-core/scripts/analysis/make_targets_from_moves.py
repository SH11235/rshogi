#!/usr/bin/env python3
"""
usi_gauntlet の構造化ログ（moves.jsonl）から評価スパイクを検出し、
数手遡りの局面を run_eval_targets.py 向けの targets.json 形式に変換するツール。

想定入力:
  - cargo run -p tools --bin usi_gauntlet --release -- --log-moves ... --out runs/...
  - 上記で生成される runs/.../moves.jsonl （1行=1手の JSON）

出力:
  - <out>/targets.json  （run_eval_targets.py が読む形式: { "targets": [ {tag, pre_position, ...}, ... ] }）
  - <out>/summary.txt   （簡易サマリ: ゲームごとのスパイク数等）

スパイク検出の方針は make_targets_from_logs.py と概ね同一で、
eval(cp/mate) の隣接差分が threshold 以上になった箇所をスパイクとみなす。
"""

import argparse
import json
import os
from typing import Dict, List, Tuple, Optional


def compute_spikes(evals: List[int], threshold: int) -> List[Tuple[int, int]]:
    """評価列からスパイク候補 (index, delta) を抽出する。index は 1 始まり。"""
    spikes: List[Tuple[int, int]] = []
    prev: Optional[int] = None
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
    """USI の position 本体（startpos / sfen ...）から末尾の手を back_plies 手ぶん削る。

    例:
      pos_line = "startpos moves 7g7f 3c3d", back_plies=1
        -> "startpos moves 7g7f"
      pos_line = "sfen ...", back_plies>=1
        -> "sfen ..."（moves が無ければそのまま）
    """
    if not pos_line:
        return None
    if " moves " not in pos_line or back_plies <= 0:
        return pos_line
    head, moves = pos_line.split(" moves ", 1)
    toks = [t for t in moves.strip().split() if t]
    if len(toks) >= back_plies:
        toks = toks[: -back_plies]
    else:
        toks = []
    return f"{head} moves {' '.join(toks)}" if toks else head


def make_pos_after(pos_body: str, bestmove: str) -> str:
    """position 本体と bestmove から、着手後の position 本体を構成する。

    - pos_body は "startpos ..." または "sfen ... [moves ...]" を想定。
    - bestmove は "resign" 等も取り得るが、その場合は pos_body をそのまま返す。
    """
    mv = bestmove.strip()
    if not mv or mv in ("resign", "none"):
        return pos_body
    if " moves " in pos_body:
        head, moves = pos_body.split(" moves ", 1)
        toks = [t for t in moves.strip().split() if t]
        toks.append(mv)
        return f"{head} moves {' '.join(toks)}"
    return f"{pos_body} moves {mv}"


def main() -> None:
    ap = argparse.ArgumentParser(
        description="usi_gauntlet の moves.jsonl からスパイク局面を抽出し、targets.json を生成"
    )
    ap.add_argument(
        "moves",
        nargs="+",
        help="usi_gauntlet --log-moves で生成した moves.jsonl（複数指定可）",
    )
    ap.add_argument(
        "--threshold",
        type=int,
        default=250,
        help="abs(cp) スパイク閾値（既定: 250）",
    )
    ap.add_argument(
        "--topk",
        type=int,
        default=10,
        help="各 (ファイル×ゲーム) の上位K件に制限（0=無制限, 既定:10）",
    )
    ap.add_argument(
        "--back-min",
        type=int,
        default=2,
        help="遡り最小手数（既定:2）",
    )
    ap.add_argument(
        "--back-max",
        type=int,
        default=5,
        help="遡り最大手数（既定:5）",
    )
    ap.add_argument(
        "--side",
        choices=["cand", "base", "both"],
        default="cand",
        help="どちら側の手を対象とするか（既定: cand）",
    )
    ap.add_argument(
        "--out",
        required=True,
        help="出力ディレクトリ（targets.json / summary.txt を出力）",
    )
    args = ap.parse_args()

    os.makedirs(args.out, exist_ok=True)

    # (ファイル名, game_index) -> [record,...]
    by_group: Dict[Tuple[str, int], List[dict]] = {}
    summary_lines: List[str] = []

    for path in args.moves:
        base = os.path.basename(path)
        try:
            with open(path, "r", encoding="utf-8") as f:
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        rec = json.loads(line)
                    except json.JSONDecodeError:
                        continue
                    side = rec.get("side")
                    if args.side != "both" and side != args.side:
                        continue
                    game_index = rec.get("game_index")
                    if game_index is None:
                        continue
                    key = (base, int(game_index))
                    by_group.setdefault(key, []).append(rec)
        except FileNotFoundError:
            summary_lines.append(f"SKIP(not found): {path}")
            continue

    uniq_positions = set()
    targets: List[dict] = []

    for (base, game_index), recs in sorted(by_group.items(), key=lambda x: (x[0][0], x[0][1])):
        # ply 昇順に並べる（念のため）
        recs_sorted = sorted(recs, key=lambda r: int(r.get("ply", 0)))
        evals: List[int] = []
        meta: List[dict] = []
        cur_eval: Optional[int] = None

        for r in recs_sorted:
            cp = r.get("eval_cp")
            mate = r.get("eval_mate")
            cp_val: Optional[int] = None
            if isinstance(cp, int):
                cp_val = cp
            elif isinstance(mate, int):
                cp_val = 100000 if mate > 0 else -100000
            if cp_val is not None:
                cur_eval = cp_val
            if cur_eval is None:
                cur_eval = evals[-1] if evals else 0
            evals.append(cur_eval)
            meta.append(
                {
                    "ply_abs": int(r.get("ply", 0)),
                    "pos_body": r.get("position") or "",
                    "bestmove": r.get("bestmove") or "",
                    "side": r.get("side"),
                    "cand_black": bool(r.get("cand_black", False)),
                }
            )

        if not evals:
            summary_lines.append(
                f"{base}: game={game_index} moves=0 spikes=0 (threshold={args.threshold})"
            )
            continue

        spikes = compute_spikes(evals, args.threshold)
        if args.topk and len(spikes) > args.topk:
            spikes = sorted(spikes, key=lambda x: abs(x[1]), reverse=True)[: args.topk]

        summary_lines.append(
            f"{base}: game={game_index} moves={len(evals)} spikes={len(spikes)} (threshold={args.threshold})"
        )

        stem = os.path.splitext(base)[0]
        for (idx, delta) in spikes:
            if idx < 1 or idx > len(meta):
                continue
            m = meta[idx - 1]
            pos_after = make_pos_after(m["pos_body"], m["bestmove"])
            ply_abs = m["ply_abs"]
            for back in range(args.back_min, args.back_max + 1):
                pos = chop_moves(pos_after, back)
                if not pos:
                    continue
                if pos in uniq_positions:
                    continue
                uniq_positions.add(pos)
                tag = f"{stem}_g{game_index}_ply{ply_abs}_back{back}"
                targets.append(
                    {
                        "tag": tag,
                        "pre_position": pos,
                        "origin_log": base,
                        "origin_game_index": game_index,
                        "origin_ply": ply_abs,
                        "origin_delta": delta,
                        "back_plies": back,
                        "origin_side": m.get("side"),
                        "origin_cand_black": m.get("cand_black"),
                    }
                )

    out_json = os.path.join(args.out, "targets.json")
    with open(out_json, "w", encoding="utf-8") as wf:
        json.dump({"targets": targets}, wf, ensure_ascii=False, indent=2)

    summary_lines.append(f"unique_targets={len(targets)}")
    with open(os.path.join(args.out, "summary.txt"), "w", encoding="utf-8") as wf:
        wf.write("\n".join(summary_lines) + "\n")

    print(f"wrote targets={len(targets)} -> {out_json}")


if __name__ == "__main__":
    main()

