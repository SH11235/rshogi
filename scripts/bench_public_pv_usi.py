#!/usr/bin/env python3
"""NNUE/production バイナリで公開PVの処理量・USI終了遅延を比較する。"""
import argparse
import csv
import json
from pathlib import Path
import queue
import subprocess
import threading
import time


class Engine:
    def __init__(self, binary, model, progress, threads, cpus=""):
        command = [str(Path(binary).resolve())]
        if cpus:
            command = ["taskset", "-c", cpus] + command
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.STDOUT, text=True, bufsize=1)
        self.lines = queue.Queue()
        def reader():
            for line in self.process.stdout:
                self.lines.put((time.perf_counter_ns(), line.strip()))
            self.lines.put((time.perf_counter_ns(), None))
        self.reader = threading.Thread(target=reader, daemon=True)
        self.reader.start()
        try:
            self.send("usi")
            self.until("usiok")
            for name, value in [("Hash", 256), ("Threads", threads), ("EvalFile", model),
                                ("FV_SCALE", 14), ("LS_BUCKET_MODE", "progresskpabs"),
                                ("LS_PROGRESS_BUCKETS", 9), ("LS_PROGRESS_COEFF", progress)]:
                self.send(f"setoption name {name} value {value}")
            self.send("isready")
            self.setup = self.until("readyok")
        except BaseException:
            self.close()
            raise

    def send(self, command):
        stamp = time.perf_counter_ns()
        self.process.stdin.write(command + "\n")
        self.process.stdin.flush()
        return stamp

    def until(self, prefix):
        result = []
        deadline = time.monotonic() + 60
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f"engine did not emit {prefix!r}")
            stamp, line = self.lines.get(timeout=remaining)
            if line is None:
                raise RuntimeError(f"engine exited: {result[-10:]}")
            result.append((stamp, line))
            if "info string Error" in line or "Unknown option" in line:
                raise RuntimeError(line)
            if line.startswith("info string {"):
                try:
                    payload = json.loads(line[len("info string "):])
                except json.JSONDecodeError:
                    payload = {}
                if payload.get("type") == "error":
                    raise RuntimeError(line)
            if line.startswith(prefix):
                return result

    def reset(self, multipv):
        self.send("usinewgame")
        self.send(f"setoption name MultiPV value {multipv}")
        self.send("isready")
        self.until("readyok")

    def search(self, position, go, stop_ms=None):
        self.send(position)
        start = self.send(go)
        stop = None
        if stop_ms is not None:
            time.sleep(stop_ms / 1000)
            stop = self.send("stop")
        lines = self.until("bestmove ")
        infos = [line.split() for _, line in lines if line.startswith("info depth ")]
        if not infos or any(name not in infos[-1] for name in ["nodes", "depth", "score", "time"]):
            raise RuntimeError(f"missing search metrics: {lines[-10:]}")
        last = infos[-1]
        def field(name, default="0"):
            return last[last.index(name) + 1] if name in last else default
        pv_lengths = [len(words) - words.index("pv") - 1 if "pv" in words else 0 for words in infos]
        return dict(elapsed_ns=lines[-1][0] - start,
                    stop_lag_ns=lines[-1][0] - stop if stop is not None else "",
                    nodes=field("nodes"), depth=field("depth"),
                    score=" ".join(last[last.index("score") + 1:last.index("score") + 3]) if "score" in last else "",
                    bestmove=lines[-1][1].split()[1], callbacks=len(infos),
                    max_pv=max(pv_lengths, default=0), last_pv=pv_lengths[-1] if pv_lengths else 0,
                    engine_time_ms=field("time"))

    def close(self):
        if self.process.poll() is None:
            try:
                self.send("quit")
                self.process.wait(timeout=5)
            except (BrokenPipeError, subprocess.TimeoutExpired):
                self.process.kill()
                self.process.wait()
        self.reader.join(timeout=2)
        self.process.stdin.close()
        self.process.stdout.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline")
    parser.add_argument("fixed")
    parser.add_argument("model")
    parser.add_argument("progress")
    parser.add_argument("positions", type=Path, help="label/position を含むJSON配列")
    parser.add_argument("output", type=Path)
    parser.add_argument("--blocks", type=int, default=6)
    parser.add_argument("--movetime", type=int, default=1000)
    parser.add_argument("--stop-ms", type=int, default=250)
    parser.add_argument("--cpus", default="2,3,4,5")
    args = parser.parse_args()
    if args.blocks <= 0 or args.movetime <= 0 or args.stop_ms <= 0:
        parser.error("blocks and time limits must be positive")
    inputs = [Path(p).resolve() for p in [args.baseline, args.fixed, args.model, args.progress, args.positions]]
    if args.output.resolve() in inputs:
        parser.error("output must differ from inputs")
    positions = json.loads(args.positions.read_text())
    fields = ["variant", "block", "order", "mode", "threads", "multipv", "position",
              "elapsed_ns", "stop_lag_ns", "nodes", "depth", "score", "bestmove", "callbacks",
              "max_pv", "last_pv", "engine_time_ms"]
    with args.output.open("w", newline="") as output:
        writer = csv.DictWriter(output, fieldnames=fields, lineterminator="\n")
        writer.writeheader()
        for threads in [1, 4]:
            engines = {}
            try:
                for variant in ["baseline", "fixed"]:
                    cpus = args.cpus.split(",")[0] if threads == 1 else args.cpus
                    engines[variant] = Engine(getattr(args, variant), args.model, args.progress, threads, cpus)
                    engines[variant].reset(1)
                    engines[variant].search("position startpos", "go nodes 10000")
                for mode in (["nodes", "time", "stop"] if threads == 1 else ["time", "stop"]):
                    blocks = 1 if mode == "nodes" else args.blocks
                    for block in range(blocks):
                        for order, variant in enumerate(["baseline", "fixed", "fixed", "baseline"]):
                            engine = engines[variant]
                            for multipv in [1, 8]:
                                for pos in positions:
                                    engine.reset(multipv)
                                    go = {"nodes": "go nodes 100000", "time": f"go movetime {args.movetime}",
                                          "stop": "go infinite"}[mode]
                                    row = engine.search(pos["position"], go, args.stop_ms if mode == "stop" else None)
                                    row.update(variant=variant, block=block, order=order, mode=mode,
                                               threads=threads, multipv=multipv, position=pos["label"])
                                    writer.writerow(row)
                                    output.flush()
                        print(f"threads={threads} mode={mode} block={block + 1}/{blocks}", flush=True)
            finally:
                for engine in engines.values():
                    engine.close()


if __name__ == "__main__":
    main()
