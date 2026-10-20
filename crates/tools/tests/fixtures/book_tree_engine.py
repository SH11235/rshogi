#!/usr/bin/env python3
"""局面表と遅延・停止・一度だけの切断を指定できる USI fixture。"""
import json
import os
import sys
import time

config = {}
key = ""
newgame = False
ready = False


def emit(line):
    print(line, flush=True)


for raw in sys.stdin:
    line = raw.strip()
    if config.get("log"):
        with open(config["log"], "a", encoding="utf-8") as log:
            log.write(json.dumps([os.getpid(), line]) + "\n")
    if line == "usi":
        for name in ["FixtureFile", "EvalFile", "Threads", "USI_Hash", "MultiPV"]:
            emit("option name " + name + " type string default empty")
        emit("usiok")
    elif line.startswith("setoption name FixtureFile value "):
        with open(line.split(" value ", 1)[1], encoding="utf-8") as source:
            config = json.load(source)
    elif line == "usinewgame":
        newgame = True
        ready = False
    elif line == "isready":
        ready = newgame
        emit("readyok")
    elif line.startswith("position sfen "):
        key = " ".join(line.split()[2:5])
    elif line.startswith("go "):
        if not newgame or not ready:
            sys.exit(9)
        newgame = False
        if config.get("crash_always"):
            sys.exit(8)
        marker = config.get("crash_once")
        if marker:
            try:
                fd = os.open(marker, os.O_CREAT | os.O_EXCL | os.O_WRONLY)
            except FileExistsError:
                pass
            else:
                os.close(fd)
                sys.exit(7)
        rows = config["positions"][key]
        delay = config.get("delay", 0)
        start = time.monotonic()
        while time.monotonic() - start < delay:
            if config.get("stagnant"):
                emit("info depth 1 nodes 1 score cp 0 pv " + rows[0]["move"])
                emit("info string alive")
            time.sleep(0.05)
        for index, row in enumerate(rows, 1):
            emit(f"info depth {row['depth']} nodes 101 multipv {index} score cp {row['cp']} pv {row['move']}")
        emit("bestmove " + rows[0]["move"])
    elif line == "quit":
        break
