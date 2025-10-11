#!/usr/bin/env python3
# moved: scripts/analysis/run_usi_ab_test.py
import argparse
import json
import os
import re
import shlex
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path


BESTMOVE_RE = re.compile(r"^info .*score (cp|mate) (?P<score>-?\d+).*(?: pv .*)?$")
INFO_DEPTH_RE = re.compile(r"^info depth ")
BESTMOVE_LINE_RE = re.compile(r"^bestmove ")
ASP_FAIL_RE = re.compile(r"aspiration fail-(low|high)")


START_MOVES = (
    "3i4h 3c3d 3g3f 4a3b 2i3g 8c8d 4g4f 8d8e 2g2f 7a7b"
)

def parse_moves_list(arg: str):
    """Parse a list of move sequences separated by '|'"""
    if not arg:
        return [START_MOVES]
    parts = [p.strip() for p in arg.split('|') if p.strip()]
    return parts or [START_MOVES]


def run_engine(engine_path: str, options: dict, moves: str, byoyomi_ms: int,
               postdrop_eval: bool = False, postdrop_ms: int = 2000,
               extra_post_moves: str = "8e8f", timeout_sec: int = 30,
               eval_best_reply: bool = False,
               fixed_only_if_relevant: bool = True) -> dict:
    """
    Launch engine, send USI commands, collect final info/bestmove and diagnostics.
    Returns dict with keys: bestmove, final_info_score, final_info_bound, depth, seldepth,
    nps, nodes, asp_fail_count, lines (raw), postdrop_score, finalize_seen.
    """
    proc = subprocess.Popen(
        [engine_path],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
    )
    out_lines = []
    def send(cmd: str):
        assert proc.stdin is not None
        proc.stdin.write(cmd + "\n")
        proc.stdin.flush()

    # Handshake
    send("usi")
    # set options
    for k, v in options.items():
        if isinstance(v, bool):
            if v:
                send(f"setoption name {k} value true")
            else:
                send(f"setoption name {k} value false")
        else:
            send(f"setoption name {k} value {v}")
    send("isready")

    bestmove = None
    final_info_score = None
    final_info_bound = None
    depth = None
    seldepth = None
    nps = None
    nodes = None
    asp_fail_count = 0
    finalize_checked = False
    finalize_switched = False
    finalize_seen = False

    t0 = time.time()
    # Read until readyok
    while True:
        if time.time() - t0 > timeout_sec:
            proc.kill()
            raise RuntimeError("USI handshake timeout")
        line = proc.stdout.readline()
        if not line:
            continue
        line = line.rstrip("\n")
        out_lines.append(line)
        if line.startswith("readyok"):
            break

    # New game and position
    send("usinewgame")
    send("position startpos moves " + moves)
    send(f"go btime 0 wtime 0 byoyomi {byoyomi_ms}")

    last_info_score = None
    # Read until bestmove
    t1 = time.time()
    while True:
        if time.time() - t1 > timeout_sec:
            proc.kill()
            raise RuntimeError("go() timeout")
        line = proc.stdout.readline()
        if not line:
            continue
        line = line.rstrip("\n")
        out_lines.append(line)
        if ASP_FAIL_RE.search(line):
            asp_fail_count += 1
        if "finalize_event" in line:
            finalize_seen = True
        if "sanity_checked=1" in line:
            finalize_checked = True
            if "switched=1" in line:
                finalize_switched = True
        if line.startswith("info depth "):
            # Try to extract score/metrics from same or subsequent info
            m = re.search(r"score (cp|mate) (-?\d+)", line)
            if m:
                last_info_score = (m.group(1), int(m.group(2)))
            dm = re.search(r"depth (\d+)", line)
            if dm:
                depth = int(dm.group(1))
            sm = re.search(r"seldepth (\d+)", line)
            if sm:
                seldepth = int(sm.group(1))
            nm = re.search(r"nodes (\d+)", line)
            if nm:
                nodes = int(nm.group(1))
            npsm = re.search(r"nps (\d+)", line)
            if npsm:
                nps = int(npsm.group(1))
        if BESTMOVE_LINE_RE.match(line):
            # finalize with last info score
            bestmove = line.split()[1]
            if last_info_score:
                kind, val = last_info_score
                if kind == "cp":
                    final_info_score = val
                    final_info_bound = "cp"
                else:
                    # mate score: approximate to large cp with sign
                    final_info_score = 30000 if val > 0 else -30000
                    final_info_bound = "mate"
            break

    postdrop_score = None
    postbest_score = None
    if postdrop_eval and bestmove is not None:
        # quick post-drop probing
        do_fixed = True
        if fixed_only_if_relevant and bestmove != "8g8f":
            do_fixed = False
        if do_fixed:
            send("position startpos moves " + moves + " " + bestmove + " " + extra_post_moves)
            send(f"go btime 0 wtime 0 byoyomi {postdrop_ms}")
            t2 = time.time()
            last_post_info = None
            while True:
                if time.time() - t2 > timeout_sec:
                    break
                line = proc.stdout.readline()
                if not line:
                    continue
                line = line.rstrip("\n")
                out_lines.append(line)
                if line.startswith("info depth "):
                    m = re.search(r"score (cp|mate) (-?\d+)", line)
                    if m:
                        last_post_info = (m.group(1), int(m.group(2)))
                if BESTMOVE_LINE_RE.match(line):
                    if last_post_info:
                        kind, val = last_post_info
                        postdrop_score = val if kind == "cp" else (30000 if val > 0 else -30000)
                    break

        if eval_best_reply:
            # Opponent best reply: after our bestmove, opponent to move
            send("position startpos moves " + moves + " " + bestmove)
            send(f"go btime 0 wtime 0 byoyomi {postdrop_ms}")
            t3 = time.time()
            last_best_info = None
            while True:
                if time.time() - t3 > timeout_sec:
                    break
                line = proc.stdout.readline()
                if not line:
                    continue
                line = line.rstrip("\n")
                out_lines.append(line)
                if line.startswith("info depth "):
                    m = re.search(r"score (cp|mate) (-?\d+)", line)
                    if m:
                        last_best_info = (m.group(1), int(m.group(2)))
                if BESTMOVE_LINE_RE.match(line):
                    if last_best_info:
                        kind, val = last_best_info
                        # Opponent perspective -> invert to our perspective
                        v = val if kind == "cp" else (30000 if val > 0 else -30000)
                        postbest_score = -v
                    break

    try:
        send("quit")
    except Exception:
        pass
    try:
        proc.wait(timeout=2)
    except subprocess.TimeoutExpired:
        proc.kill()

    elapsed_ms = int((time.time() - t1) * 1000)
    return {
        "bestmove": bestmove,
        "final_score": final_info_score,
        "final_bound": final_info_bound,
        "depth": depth,
        "seldepth": seldepth,
        "nps": nps,
        "nodes": nodes,
        "elapsed_ms": elapsed_ms,
        "asp_fail": asp_fail_count,
        "finalize_seen": finalize_seen,
        "finalize_checked": finalize_checked,
        "finalize_switched": finalize_switched,
        "postdrop_score": postdrop_score,
        "postbest_score": postbest_score,
        "lines": out_lines,
    }


def build_option_sets(threads: int, warmup: bool, set_kind: str) -> dict:
    base = {
        "Threads": threads,
        # Keep consistent MultiPV for diagnostics across sets
        "MultiPV": 2,
    }
    if warmup:
        base.update({"Warmup.Ms": 300, "Warmup.PrevMoves": 4})
    # Baseline
    if set_kind == "baseline":
        return base
    # Finalize-only
    if set_kind == "finalize":
        opt = base.copy()
        opt.update({
            "FinalizeSanity.SwitchMarginCp": 30,
            "FinalizeSanity.BudgetMs": 10,
            "FinalizeSanity.OppSEE_MinCp": 100,
        })
        return opt
    # Root-only
    if set_kind == "root":
        opt = base.copy()
        opt.update({
            "RootSeeGate": True,
            "RootSeeGate.XSEE": 100,
            "PostVerify": True,
            "PostVerify.YDrop": 300,
        })
        return opt
    # Both (default from previous run)
    if set_kind == "both":
        opt = base.copy()
        opt.update({
            "FinalizeSanity.SwitchMarginCp": 30,
            "FinalizeSanity.BudgetMs": 10,
            "FinalizeSanity.OppSEE_MinCp": 100,
            "RootSeeGate": True,
            "RootSeeGate.XSEE": 100,
            "PostVerify": True,
            "PostVerify.YDrop": 300,
        })
        return opt
    # Balanced (proposal): YDrop=200, Switch=30, OppSEE=100, BudgetMs T8/T1=10/5
    if set_kind == "balanced":
        opt = base.copy()
        opt.update({
            "RootSeeGate": True,
            "RootSeeGate.XSEE": 100,
            "PostVerify": True,
            "PostVerify.YDrop": 200,
            "FinalizeSanity.SwitchMarginCp": 30,
            "FinalizeSanity.OppSEE_MinCp": 100,
            # Budget per threads
            "FinalizeSanity.BudgetMs": 10 if threads > 1 else 5,
        })
        return opt
    # Perf-friendly: YDrop=300, Switch=40, OppSEE=150, BudgetMs T8/T1=8/4
    if set_kind == "perf":
        opt = base.copy()
        opt.update({
            "RootSeeGate": True,
            "RootSeeGate.XSEE": 100,
            "PostVerify": True,
            "PostVerify.YDrop": 300,
            "FinalizeSanity.SwitchMarginCp": 40,
            "FinalizeSanity.OppSEE_MinCp": 150,
            "FinalizeSanity.BudgetMs": 8 if threads > 1 else 4,
        })
        return opt
    # Set A (T1, finalize-lite + RootSeeGate, PostVerify OFF)
    if set_kind.lower() == "seta":
        opt = base.copy()
        opt.update({
            "RootSeeGate": True,
            "RootSeeGate.XSEE": 100,
            "PostVerify": False,
            "FinalizeSanity.SwitchMarginCp": 35,
            "FinalizeSanity.OppSEE_MinCp": 120,
            "FinalizeSanity.BudgetMs": 4 if threads == 1 else 8,
        })
        return opt
    # Set B (T1, perf-like with PostVerify ON)
    if set_kind.lower() == "setb":
        opt = base.copy()
        opt.update({
            "RootSeeGate": True,
            "RootSeeGate.XSEE": 100,
            "PostVerify": True,
            "PostVerify.YDrop": 300,
            "FinalizeSanity.SwitchMarginCp": 35,
            "FinalizeSanity.OppSEE_MinCp": 120,
            "FinalizeSanity.BudgetMs": 4 if threads == 1 else 8,
        })
        return opt
    raise ValueError(set_kind)


def summarize(results):
    import statistics as stats
    summs = {}
    for key, runs in results.items():
        best_8g8f = sum(1 for r in runs if r.get("bestmove") == "8g8f")
        post_scores = [r.get("postdrop_score") for r in runs if r.get("postdrop_score") is not None]
        postbest_scores = [r.get("postbest_score") for r in runs if r.get("postbest_score") is not None]
        # worst-case per trial among fixed 8e8f vs opponent best reply
        postmax_scores = []
        for r in runs:
            a = r.get("postdrop_score")
            b = r.get("postbest_score")
            cand = [v for v in (a, b) if v is not None]
            if cand:
                postmax_scores.append(min(cand))
        depths = [r.get("depth") for r in runs if r.get("depth") is not None]
        nps = [r.get("nps") for r in runs if r.get("nps") is not None]
        elaps = [r.get("elapsed_ms") for r in runs if r.get("elapsed_ms") is not None]
        asp = sum(r.get("asp_fail") or 0 for r in runs)
        fin_chk = sum(1 for r in runs if r.get("finalize_checked"))
        fin_swi = sum(1 for r in runs if r.get("finalize_switched"))
        def pxx(xs, q):
            if not xs:
                return None
            ys = sorted(xs)
            idx = max(0, min(len(ys)-1, int(q*len(ys))-1))
            return ys[idx]
        summs[key] = {
            "count": len(runs),
            "best_8g8f_rate": best_8g8f / max(1, len(runs)),
            "postdrop_avg": (stats.mean(post_scores) if post_scores else None),
            "postdrop_min": (min(post_scores) if post_scores else None),
            "postbest_avg": (stats.mean(postbest_scores) if postbest_scores else None),
            "postbest_min": (min(postbest_scores) if postbest_scores else None),
            "postmax_avg": (stats.mean(postmax_scores) if postmax_scores else None),
            "postmax_min": (min(postmax_scores) if postmax_scores else None),
            "depth_avg": (stats.mean(depths) if depths else None),
            "nps_avg": (stats.mean(nps) if nps else None),
            "elapsed_avg_ms": (stats.mean(elaps) if elaps else None),
            "elapsed_p95_ms": pxx(elaps, 0.95),
            "elapsed_p99_ms": pxx(elaps, 0.99),
            "asp_fail_total": asp,
            "finalize_checked_rate": fin_chk / max(1, len(runs)),
            "finalize_switched_rate": fin_swi / max(1, len(runs)),
        }
    return summs


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--engine", default=str(Path("target/release/engine-usi")))
    ap.add_argument("--threads", nargs=2, default=["1", "8"], help="e.g. 1 8")
    ap.add_argument("--trials", type=int, default=10)
    ap.add_argument("--warmup", choices=["on", "off"], default="on")
    ap.add_argument("--byoyomi", type=int, default=10000)
    ap.add_argument("--postms", type=int, default=2000)
    ap.add_argument("--outdir", default=None)
    ap.add_argument("--moves-list", default=None, help="pipe-separated list of move sequences; default is built-in START_MOVES")
    ap.add_argument("--sets", default="baseline,finalize,root,both", help="comma list e.g. balanced,perf,setA,setB")
    ap.add_argument("--eval-best-reply", action="store_true", help="probe opponent best reply as well")
    ap.add_argument("--strict-fixed", action="store_true", help="always evaluate fixed reply (8e8f); otherwise only when our bestmove is 8g8f")
    # Simple per-run overrides (applied to all sets)
    ap.add_argument("--override-ydrop", type=int, default=None)
    ap.add_argument("--override-budgetms", type=int, default=None)
    ap.add_argument("--override-switch", type=int, default=None)
    ap.add_argument("--override-oppsee", type=int, default=None)
    ap.add_argument("--override-kingalt-min", type=int, default=None)
    ap.add_argument("--override-allow-see-lt0-alt", choices=["on","off"], default=None)
    ap.add_argument("--override-multipv", type=int, default=None)
    ap.add_argument("--override-rootseegate", choices=["on","off"], default=None)
    ap.add_argument("--override-postverify", choices=["on","off"], default=None)
    ap.add_argument("--override-postverify-requirepass", choices=["on","off"], default=None)
    ap.add_argument("--override-postverify-extendms", type=int, default=None)
    args = ap.parse_args()

    engine = args.engine
    if not Path(engine).exists():
        print(f"[ERROR] engine not found: {engine}", file=sys.stderr)
        sys.exit(2)

    ts = datetime.now().strftime("%Y%m%d_%H%M%S")
    outdir = Path(args.outdir or Path("runs") / f"ab_test_{ts}")
    outdir.mkdir(parents=True, exist_ok=True)

    sets = [s.strip() for s in args.sets.split(",") if s.strip()]
    threads_list = [int(x) for x in args.threads]
    warmup = args.warmup == "on"

    all_results = {}
    moves_list = parse_moves_list(args.moves_list) if args.moves_list else [START_MOVES]

    for th in threads_list:
        for kind in sets:
            for pos_idx, mvseq in enumerate(moves_list):
                key = f"{kind}_T{th}_P{pos_idx}"
                all_results[key] = []
                opts = build_option_sets(th, warmup, kind)
                # Apply overrides if requested
                if args.override_multipv is not None:
                    opts["MultiPV"] = int(args.override_multipv)
                if args.override_budgetms is not None:
                    opts["FinalizeSanity.BudgetMs"] = int(args.override_budgetms)
                if args.override_switch is not None:
                    opts["FinalizeSanity.SwitchMarginCp"] = int(args.override_switch)
                if args.override_oppsee is not None:
                    opts["FinalizeSanity.OppSEE_MinCp"] = int(args.override_oppsee)
                if args.override_kingalt_min is not None:
                    opts["FinalizeSanity.KingAltMinGainCp"] = int(args.override_kingalt_min)
                if args.override_allow_see_lt0_alt is not None:
                    opts["FinalizeSanity.AllowSEElt0Alt"] = (args.override_allow_see_lt0_alt == "on")
                if args.override_ydrop is not None:
                    opts["PostVerify.YDrop"] = int(args.override_ydrop)
                if args.override_rootseegate is not None:
                    opts["RootSeeGate"] = (args.override_rootseegate == "on")
                if args.override_postverify is not None:
                    opts["PostVerify"] = (args.override_postverify == "on")
                if args.override_postverify_requirepass is not None:
                    opts["PostVerify.RequirePass"] = (args.override_postverify_requirepass == "on")
                if args.override_postverify_extendms is not None:
                    opts["PostVerify.ExtendMs"] = int(args.override_postverify_extendms)
                for i in range(args.trials):
                    try:
                        res = run_engine(
                            engine,
                            opts,
                            mvseq,
                            args.byoyomi,
                            postdrop_eval=True,
                            postdrop_ms=args.postms,
                            timeout_sec=max(30, args.byoyomi // 1000 + 10),
                            eval_best_reply=args.eval_best_reply,
                            fixed_only_if_relevant=(not args.strict_fixed),
                        )
                    except Exception as e:
                        res = {"error": str(e)}
                    res["trial"] = i
                    res["set"] = key
                    res["pos_idx"] = pos_idx
                    res["moves"] = mvseq
                    all_results[key].append(res)
                    # persist incremental logs
                    with open(outdir / f"{key}_trial{i}.log", "w", encoding="utf-8") as f:
                        for ln in res.get("lines", []):
                            f.write(ln + "\n")
                # per-set summary JSON
                with open(outdir / f"{key}_raw.json", "w", encoding="utf-8") as f:
                    json.dump(all_results[key], f, ensure_ascii=False, indent=2)

    summary = summarize(all_results)
    with open(outdir / "summary.json", "w", encoding="utf-8") as f:
        json.dump(summary, f, ensure_ascii=False, indent=2)

    # pretty print
    print("== Summary ==")
    for k, v in summary.items():
        print(
            f"{k}: cnt={v['count']} 8g8f={v['best_8g8f_rate']:.2f} "
            f"postFixed(avg/min)={v['postdrop_avg']} / {v['postdrop_min']} "
            f"postBest(avg/min)={v['postbest_avg']} / {v['postbest_min']} "
            f"postMax(avg/min)={v['postmax_avg']} / {v['postmax_min']} "
            f"depth_avg={v['depth_avg']} nps_avg={v['nps_avg']} asp_fail={v['asp_fail_total']} "
            f"finalize(check/switched)={v['finalize_checked_rate']:.2f}/{v['finalize_switched_rate']:.2f}"
        )


if __name__ == "__main__":
    main()
