import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import type { Miniflare } from 'miniflare';
import {
  CsaClient,
  createMiniflare,
  getFloodgateHistoryBucket,
  getKifuBucket,
  makeTempPersistRoot,
} from '../miniflare_smoke/harness.ts';

// 終局処理の副作用操作 N 番目ごとに障害を注入し、実際に予約された alarm だけで
// 再開させたうえで、終局処理の不変条件を検査する。

type Mode = 'fail' | 'crash';

interface RoomState {
  fired: { fired: boolean; failed?: boolean } | null;
  active: number;
  ops: string[];
  injectedAt: { index: number; name: string } | null;
  finished: { result_code: string; exported_at_ms: number | null } | null;
  finalizing: unknown;
  kind: string | null;
  alarm: number | null;
  moves: number;
}

interface Game {
  gameId: string;
  black: CsaClient;
  white: CsaClient;
  control: (command: object) => Promise<RoomState>;
}

interface Scenario {
  name: string;
  trigger: (game: Game) => Promise<void>;
}

interface Observation {
  state: RoomState;
  lines: { black: string[]; white: string[] };
  closed: { black: number | undefined; white: number | undefined };
  kifuMoves: number | null;
  historyObjects: number;
  liveEntries: number;
}

interface Violation {
  scenario: string;
  mode: Mode;
  at: number;
  op: string;
  problem: string;
}

const CYCLE = ['+5958OU', '-5152OU', '+5859OU', '-5251OU'] as const;

const SCENARIOS: Scenario[] = [
  { name: 'sennichite', trigger: async (g) => g.white.send(CYCLE[3]) },
  { name: 'toryo', trigger: async (g) => g.white.send('%TORYO') },
  { name: 'disconnect', trigger: async (g) => { await g.white.close(); } },
];

const MODES: Mode[] = ['fail', 'crash'];

describe('終局処理の網羅障害注入', () => {
  let mf: Miniflare;
  let cleanup: () => Promise<void>;
  let roomSeq = 0;
  const violations: Violation[] = [];
  const inputLost: Array<Omit<Violation, 'problem'>> = [];

  beforeAll(async () => {
    const persist = await makeTempPersistRoot();
    cleanup = persist.cleanup;
    mf = await createMiniflare({
      persistRoot: persist.path,
      allowFloodgateFeatures: true,
      scriptPath: resolve(import.meta.dirname, 'exhaustive-worker.mjs'),
    });
  });

  afterAll(async () => {
    const report = process.env.FINALIZATION_EXHAUSTIVE_REPORT;
    if (report) await writeFile(report, JSON.stringify({ violations, inputLost }, null, 2));
    await mf?.dispose();
    await cleanup?.();
  });

  async function startGame(): Promise<Game> {
    const roomId = `exhaustive-${roomSeq++}`;
    const ns = await mf.getDurableObjectNamespace('GAME_ROOM') as unknown as {
      idFromName(name: string): unknown;
      get(id: unknown): { fetch(url: string, init: RequestInit): Promise<Response> };
    };
    const stub = ns.get(ns.idFromName(roomId));
    const control = async (command: object) => {
      const response = await stub.fetch('https://test/__test', { method: 'POST', body: JSON.stringify(command) });
      if (!response.ok) throw new Error(await response.text());
      return await response.json() as RoomState;
    };
    const black = await CsaClient.connect(mf, roomId);
    black.send(`LOGIN black+${roomId}+black pw`);
    await black.recvLine();
    const white = await CsaClient.connect(mf, roomId);
    white.send(`LOGIN white+${roomId}+white pw`);
    await white.recvLine();
    await black.drainGameSummary();
    await white.drainGameSummary();
    black.send('AGREE');
    white.send('AGREE');
    const gameId = (await black.recvUntil(l => l.startsWith('START:'))).at(-1)!.slice('START:'.length);
    await white.recvUntil(l => l.startsWith('START:'));
    for (let i = 0; i < 11; i++) {
      const line = CYCLE[i % 4]!;
      (i % 2 === 0 ? black : white).send(line);
      await black.recvUntil(l => l.startsWith(line));
      await white.recvUntil(l => l.startsWith(line));
    }
    return { gameId, black, white, control };
  }

  async function settle(game: Game): Promise<RoomState> {
    let idle = 0;
    for (let i = 0; i < 400; i++) {
      await new Promise(r => setTimeout(r, 5));
      const state = await game.control({});
      idle = state.active === 0 ? idle + 1 : 0;
      if (idle >= 3) return state;
    }
    throw new Error('ハンドラが終わらない');
  }

  // 予約済みの alarm だけを発火させ、確定と export 完了まで進める。
  async function drive(game: Game): Promise<RoomState> {
    let state = await settle(game);
    for (let fires = 0; fires < 30; fires++) {
      if (state.finished && state.finished.exported_at_ms !== null) break;
      if (state.alarm === null) break;
      if (!state.finished && !state.finalizing && fires >= 2) break;
      await game.control({ fireAlarm: true });
      state = await settle(game);
    }
    return state;
  }

  async function drain(client: CsaClient): Promise<string[]> {
    const lines: string[] = [];
    for (;;) {
      try {
        lines.push((await client.recvLine(50)).replace(/,T\d+$/, ''));
      } catch {
        return lines;
      }
    }
  }

  async function observe(game: Game, state: RoomState): Promise<Observation> {
    await new Promise(r => setTimeout(r, 30));
    const kifu = await getKifuBucket(mf);
    const kifuKey = (await kifu.list()).objects.find(o => o.key.endsWith('.csa') && o.key.includes(game.gameId))?.key;
    const kifuText = kifuKey ? await (await kifu.get(kifuKey))!.text() : null;
    const live = (await kifu.list({ prefix: 'live-games-index/' })).objects.filter(o => o.key.includes(game.gameId));
    const history = (await (await getFloodgateHistoryBucket(mf)).list()).objects.filter(o => o.key.includes(game.gameId));
    return {
      state,
      lines: { black: await drain(game.black), white: await drain(game.white) },
      closed: { black: game.black.closeInfo()?.code, white: game.white.closeInfo()?.code },
      kifuMoves: kifuText === null ? null : kifuText.split('\n').filter(l => /^[+-]\d{4}/.test(l)).length,
      historyObjects: history.length,
      liveEntries: live.length,
    };
  }

  async function runCase(scenario: Scenario, plan: { at: number; mode: Mode }): Promise<Observation> {
    const game = await startGame();
    await game.control({ plan });
    await scenario.trigger(game);
    const state = await drive(game);
    const observation = await observe(game, state);
    await game.black.close();
    await game.white.close();
    return observation;
  }

  function isSubsequence(received: string[], expected: string[]): boolean {
    let j = 0;
    for (const line of received) {
      while (j < expected.length && expected[j] !== line) j++;
      if (j === expected.length) return false;
      j++;
    }
    return true;
  }

  function check(baseline: Observation, observed: Observation): string[] {
    const problems: string[] = [];
    const told = [...observed.lines.black, ...observed.lines.white].some(l => l.startsWith('#'));
    if (!observed.state.finished) {
      problems.push(told
        ? `確定していないのに結果を告知した (alarm=${observed.state.alarm})`
        : `確定していない (alarm=${observed.state.alarm}, finalizing=${observed.state.finalizing !== null})`);
      return problems;
    }
    if (observed.state.finished.result_code !== baseline.state.finished!.result_code) {
      problems.push(`裁定が違う: ${observed.state.finished.result_code}`);
    }
    if (observed.state.finished.exported_at_ms === null) problems.push('棋譜が export されていない');
    if (observed.kifuMoves !== baseline.kifuMoves) problems.push(`棋譜の手数が違う: ${observed.kifuMoves}`);
    if (observed.historyObjects !== baseline.historyObjects) problems.push(`floodgate 履歴が ${observed.historyObjects} 件`);
    if (observed.liveEntries !== 0) problems.push('live 一覧に残っている');
    for (const color of ['black', 'white'] as const) {
      const got = observed.lines[color];
      const want = baseline.lines[color];
      if (!isSubsequence(got, want)) {
        problems.push(`${color} が重複・余計・順序違いの行を受信: ${JSON.stringify(got)}`);
      } else if (got.length < want.length && observed.closed[color] === 1000) {
        problems.push(`${color} が正常 close なのに行が欠けた: ${JSON.stringify(got)}`);
      }
    }
    return problems;
  }

  for (const scenario of SCENARIOS) {
    for (const mode of MODES) {
      it(`${scenario.name} / ${mode}`, async () => {
        const baseline = await runCase(scenario, { at: 0, mode });
        expect(baseline.state.finished).not.toBeNull();
        const total = baseline.state.ops.length;
        for (let at = 1; at <= total; at++) {
          const observed = await runCase(scenario, { at, mode });
          const op = observed.state.injectedAt?.name ?? `(未到達) ${baseline.state.ops[at - 1]}`;
          const problems = check(baseline, observed);
          const lost = problems.length === 1 && problems[0]!.startsWith('確定していない (')
            && !observed.state.finalizing;
          if (lost) {
            inputLost.push({ scenario: scenario.name, mode, at, op });
            continue;
          }
          for (const problem of problems) violations.push({ scenario: scenario.name, mode, at, op, problem });
        }
        const mine = violations.filter(v => v.scenario === scenario.name && v.mode === mode);
        console.log(`${scenario.name}/${mode}: ${total} 地点, 違反 ${mine.length}, 入力消失 ${
          inputLost.filter(v => v.scenario === scenario.name && v.mode === mode).length}`);
        for (const v of mine) console.log(`  #${v.at} ${v.op}: ${v.problem}`);
        expect(mine).toEqual([]);
      });
    }
  }
});
