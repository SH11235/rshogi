#!/usr/bin/env python3
import json, os, re, subprocess, sys, time
import argparse

ENGINE = os.environ.get('ENGINE_BIN', 'target/release/engine-usi')


def build_common(threads:int, minthink:int):
    return {
      'Threads':str(threads),
      'USI_Hash':'1024',
      'MultiPV':'3',
      'MinThinkMs':str(minthink),
    }


def _read_until(fd, patterns, timeout_sec, out_lines):
    import select
    end=time.time()+timeout_sec
    buf=''
    while time.time() < end:
        r,_,_=select.select([fd],[],[],0.1)
        if not r:
            continue
        chunk=os.read(fd.fileno(), 4096).decode('utf-8', errors='ignore')
        if not chunk:
            break
        buf += chunk
        lines = buf.split('\n')
        buf = lines.pop() if lines else ''
        for ln in lines:
            ln = ln + '\n'
            out_lines.append(ln)
            for pat in patterns:
                if pat in ln:
                    return True
    return False


def run_one(tag, position, name, params, envadd, outdir, common_opts, byoyomi_ms:int, warmup_ms:int):
    env=os.environ.copy(); env.update(envadd or {})
    cmd=[ENGINE]
    if os.path.exists('/usr/bin/stdbuf'):
        cmd=['/usr/bin/stdbuf','-oL','-eL']+cmd
    p=subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=False, env=env, bufsize=0)
    def send(cmd):
        p.stdin.write((cmd + '\n').encode('utf-8')); p.stdin.flush()
    logfile=os.path.join(outdir, f"{tag}__{name}.log")
    out_lines=[]
    send('usi')
    _read_until(p.stdout, ['usiok'], 3.0, out_lines)
    # common options
    send('setoption name USI_Ponder value false')
    send(f'setoption name Warmup.Ms value {warmup_ms}')
    send('setoption name ForceTerminateOnHardDeadline value true')
    for k,v in common_opts.items():
        send(f'setoption name {k} value {v}')
    # params: map of SearchParams.* または RootSeeGate.* を想定
    for k,v in (params or {}).items():
        if k.startswith('RootSeeGate') or k.startswith('Root') or k.startswith('MateGate'):
            send(f'setoption name {k} value {v}')
        else:
            send(f'setoption name SearchParams.{k} value {v}')
    send('isready')
    _read_until(p.stdout, ['readyok'], 3.0, out_lines)
    send('position ' + position)
    send(f'go byoyomi {byoyomi_ms}')
    _read_until(p.stdout, [' bestmove '], (byoyomi_ms/1000.0)+6.0, out_lines)
    send('quit')
    _read_until(p.stdout, [''], 0.2, out_lines)
    try:
        p.wait(timeout=1.5)
    except subprocess.TimeoutExpired:
        p.kill()
    with open(logfile,'w',encoding='utf-8') as wf:
        wf.writelines(out_lines)
    # parse last cp
    last_cp=None; last_depth=-1
    for ln in out_lines:
        m=re.search(r'info depth (\d+).*?score cp ([+-]?\d+)', ln)
        if m:
            d=int(m.group(1)); cp=int(m.group(2))
            if d>=last_depth:
                last_depth=d; last_cp=cp
    return last_cp, last_depth


def main():
    ap=argparse.ArgumentParser(description='targets.json を 1つの param セットで評価（SPSA等向け）')
    ap.add_argument('outdir')
    ap.add_argument('--threads', type=int, default=1)
    ap.add_argument('--byoyomi', type=int, default=2000)
    ap.add_argument('--minthink', type=int, default=0)
    ap.add_argument('--warmupms', type=int, default=0)
    ap.add_argument('--params-json', required=True, help='{"name": "exp1", "params": {...}, "env": {...}}')
    args=ap.parse_args()

    outdir=args.outdir
    cfg=json.load(open(args.params_json,'r',encoding='utf-8'))
    name=cfg.get('name','exp')
    params=cfg.get('params',{})
    envadd=cfg.get('env',{})

    with open(os.path.join(outdir,'targets.json'),'r',encoding='utf-8') as f:
        targets=json.load(f)['targets']

    results=[]
    common_opts=build_common(args.threads, args.minthink)
    for t in targets:
        cp,depth=run_one(t['tag'], t['pre_position'], name, params, envadd, outdir, common_opts, args.byoyomi, args.warmupms)
        results.append({'tag':t['tag'],'profile':name,'eval_cp':cp,'depth':depth})
        print(f"{t['tag']} {name}: cp={cp} depth={depth}")
        sys.stdout.flush()
    with open(os.path.join(outdir,'summary.json'),'w',encoding='utf-8') as f:
        json.dump(results,f,ensure_ascii=False,indent=2)

if __name__=='__main__':
    if len(sys.argv)<2:
        print('usage: run_eval_targets_params.py <outdir> --params-json params.json'); sys.exit(1)
    main()

