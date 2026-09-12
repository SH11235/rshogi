import { afterAll, describe, expect, it } from 'vitest';
import { writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import type { Miniflare, WebSocket } from 'miniflare';
import {
  CsaClient,
  createMiniflare,
  getFloodgateHistoryBucket,
  getKifuBucket,
  makeTempPersistRoot,
  type HarnessOptions,
} from '../miniflare_smoke/harness.ts';
import { readLineFromWebSocket, type WebSocketLineBuffer } from '../miniflare_smoke/ws_test_helpers';

// 終局処理の副作用操作 N 番目ごとに障害を注入し、実際に予約された alarm だけで
// 再開させたうえで、終局処理の不変条件を検査する。
//
// 10 分以上かかるため CI では走らせない。終局処理 (game_room.rs の終局確定・再開・
// alarm 経路、attachment.rs の終局行の配信、persistence.rs の裁定保存と replay) を
// 変更したら `pnpm run test:finalization-exhaustive` を手動で実行する。

type Mode = 'fail' | 'crash';
type Watcher = 'black' | 'white' | 'spectator';

interface RoomState {
  fired: { fired: boolean; failed?: boolean } | null;
  active: number;
  ops: string[];
  injectedAt: { index: number; name: string } | null;
  finished: { result_code: string; exported_at_ms?: number | null } | null;
  finalizing: unknown;
  kind: string | null;
  alarm: number | null;
  moves: number;
}

interface SpectatorClient {
  buffer: WebSocketLineBuffer;
  closeCode: () => number | undefined;
  close: () => void;
}

interface Game {
  gameId: string;
  black: CsaClient;
  white: CsaClient;
  spectator: SpectatorClient | null;
  control: (command: object) => Promise<RoomState>;
}

interface Scenario {
  name: string;
  options?: Partial<HarnessOptions>;
  /** AGREE 前で止め、対局を開始しない。 */
  beforeAgree?: boolean;
  /** snapshot を受け取り終えた観戦者を 1 人つなぐ。 */
  spectator?: boolean;
  /** 計画した障害とは別に仕込む事前障害 (再試行経路へ入れるため)。 */
  preFaults?: { kifuPutFailures?: number };
  trigger: (game: Game) => Promise<void>;
}

interface Observation {
  state: RoomState;
  lines: Partial<Record<Watcher, string[]>>;
  closed: Partial<Record<Watcher, number | undefined>>;
  kifuMoves: number | null;
  historyObjects: number;
  liveEntries: number;
}

interface Finding {
  scenario: string;
  mode: Mode;
  at: number;
  op: string;
  problem: string;
}

const CYCLE = ['+5958OU', '-5152OU', '+5859OU', '-5251OU'] as const;
const sleep = (ms: number) => new Promise(r => setTimeout(r, ms));

// 持ち時間 1 秒 + 秒読み 1 秒。着手から 2 秒強で手番側の時間が尽きる。
const SHORT_CLOCK: Partial<HarnessOptions> = { clockKind: 'countdown_msec', totalTimeMs: 1000, byoyomiMs: 1000 };

const SCENARIOS: Scenario[] = [
  { name: 'sennichite', trigger: async (g) => g.white.send(CYCLE[3]) },
  { name: 'sennichite-spectator', spectator: true, trigger: async (g) => g.white.send(CYCLE[3]) },
  { name: 'sennichite-r2-retry', preFaults: { kifuPutFailures: 1 }, trigger: async (g) => g.white.send(CYCLE[3]) },
  { name: 'sennichite-r2-retry-twice', preFaults: { kifuPutFailures: 8 }, trigger: async (g) => g.white.send(CYCLE[3]) },
  { name: 'toryo', trigger: async (g) => g.white.send('%TORYO') },
  { name: 'disconnect', trigger: async (g) => { await g.white.close(); } },
  {
    name: 'time-up',
    options: SHORT_CLOCK,
    trigger: async (g) => {
      await sleep(2500);
      await g.control({ fireAlarm: true });
    },
  },
  {
    name: 'disconnect-before-agree',
    options: { agreeTimeoutSeconds: 60 },
    beforeAgree: true,
    trigger: async (g) => { await g.black.close(); },
  },
];

const MODES: Mode[] = ['fail', 'crash'];

// KEY_FINISHED 確定後の live-games-index 削除漏れは cron sweep が回収する契約。
const CRON_RECOVERED = 'live 一覧に残っている';
const INPUT_LOST_OTHER_RESULT = '入力が失われ別の裁定で確定した';

// `scenario/mode/at` を指定すると、その 1 ケースだけを実行して状態を出力する。
const ONLY_CASE = process.env.FINALIZATION_EXHAUSTIVE_CASE;

describe('終局処理の網羅障害注入', () => {
  const servers = new Map<string, { mf: Miniflare; cleanup: () => Promise<void> }>();
  let roomSeq = 0;
  const violations: Finding[] = [];
  const inputLost: Finding[] = [];
  const cronRecovered: Finding[] = [];

  afterAll(async () => {
    const report = process.env.FINALIZATION_EXHAUSTIVE_REPORT;
    if (report) {
      await writeFile(report, JSON.stringify({ violations, inputLost, cronRecovered }, null, 2));
    }
    for (const { mf, cleanup } of servers.values()) {
      await mf.dispose();
      await cleanup();
    }
  });

  async function server(scenario: Scenario): Promise<Miniflare> {
    const key = JSON.stringify(scenario.options ?? {});
    const existing = servers.get(key);
    if (existing) return existing.mf;
    const persist = await makeTempPersistRoot();
    const mf = await createMiniflare({
      persistRoot: persist.path,
      allowFloodgateFeatures: true,
      allowViewerApi: true,
      scriptPath: resolve(import.meta.dirname, 'exhaustive-worker.mjs'),
      ...scenario.options,
    });
    servers.set(key, { mf, cleanup: persist.cleanup });
    return mf;
  }

  async function connectSpectator(mf: Miniflare, gameId: string): Promise<SpectatorClient> {
    const res = await mf.dispatchFetch(`https://example.com/ws/${encodeURIComponent(gameId)}/spectate`, {
      headers: { Upgrade: 'websocket', Origin: 'https://example.com', 'CF-Connecting-IP': '127.0.0.1' },
    });
    const ws = res.webSocket as WebSocket;
    let code: number | undefined;
    ws.addEventListener('close', (ev) => { code = ev.code; });
    const buffer = readLineFromWebSocket(ws);
    ws.accept();
    ws.send(`%%MONITOR2ON ${gameId}\n`);
    while ((await buffer.takeLine(5000)) !== '##[MONITOR2] END');
    return { buffer, closeCode: () => code, close: () => ws.close() };
  }

  async function startGame(mf: Miniflare, scenario: Scenario): Promise<Game> {
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
    const summary = await black.drainGameSummary();
    await white.drainGameSummary();
    const gameId = summary.find(l => l.startsWith('Game_ID:'))!.slice('Game_ID:'.length);
    if (scenario.beforeAgree) return { gameId, black, white, spectator: null, control };
    black.send('AGREE');
    white.send('AGREE');
    await black.recvUntil(l => l.startsWith('START:'));
    await white.recvUntil(l => l.startsWith('START:'));
    for (let i = 0; i < 11; i++) {
      const line = CYCLE[i % 4]!;
      (i % 2 === 0 ? black : white).send(line);
      await black.recvUntil(l => l.startsWith(line));
      await white.recvUntil(l => l.startsWith(line));
    }
    const spectator = scenario.spectator ? await connectSpectator(mf, gameId) : null;
    return { gameId, black, white, spectator, control };
  }

  async function settle(game: Game): Promise<RoomState> {
    let idle = 0;
    for (let i = 0; i < 400; i++) {
      await sleep(5);
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
      // serde-wasm-bindgen の None は JSON 上では field 自体が省略される。
      // undefined を完了扱いすると ExportRetry の操作列を一度も検査しない。
      if (state.finished && state.finished.exported_at_ms != null) break;
      if (state.alarm === null) break;
      if (!state.finished && !state.finalizing && fires >= 2) break;
      await game.control({ fireAlarm: true });
      state = await settle(game);
    }
    return state;
  }

  async function drain(take: (timeoutMs: number) => Promise<string>): Promise<string[]> {
    const lines: string[] = [];
    for (;;) {
      try {
        lines.push((await take(50)).replace(/,T\d+$/, ''));
      } catch {
        return lines;
      }
    }
  }

  async function observe(mf: Miniflare, game: Game, state: RoomState): Promise<Observation> {
    await sleep(30);
    const kifu = await getKifuBucket(mf);
    const kifuKey = (await kifu.list()).objects.find(o => o.key.endsWith('.csa') && o.key.includes(game.gameId))?.key;
    const kifuText = kifuKey ? await (await kifu.get(kifuKey))!.text() : null;
    const live = (await kifu.list({ prefix: 'live-games-index/' })).objects.filter(o => o.key.includes(game.gameId));
    const history = (await (await getFloodgateHistoryBucket(mf)).list()).objects.filter(o => o.key.includes(game.gameId));
    const lines: Observation['lines'] = {
      black: await drain(t => game.black.recvLine(t)),
      white: await drain(t => game.white.recvLine(t)),
    };
    const closed: Observation['closed'] = {
      black: game.black.closeInfo()?.code,
      white: game.white.closeInfo()?.code,
    };
    if (game.spectator) {
      lines.spectator = await drain(t => game.spectator!.buffer.takeLine(t));
      closed.spectator = game.spectator.closeCode();
    }
    return {
      state,
      lines,
      closed,
      kifuMoves: kifuText === null ? null : kifuText.split('\n').filter(l => /^[+-]\d{4}/.test(l)).length,
      historyObjects: history.length,
      liveEntries: live.length,
    };
  }

  async function runCase(scenario: Scenario, plan: { at: number; mode: Mode }): Promise<Observation> {
    const mf = await server(scenario);
    const game = await startGame(mf, scenario);
    await game.control({ plan, preFaults: scenario.preFaults });
    await scenario.trigger(game);
    const state = await drive(game);
    const observation = await observe(mf, game, state);
    await game.black.close();
    await game.white.close();
    game.spectator?.close();
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
    const allLines = (o: Observation) => Object.values(o.lines).flat();
    const told = allLines(observed).some(l => l.startsWith('#'));
    if (!observed.state.finished) {
      problems.push(told
        ? `確定していないのに結果を告知した (alarm=${observed.state.alarm})`
        : `確定していない (alarm=${observed.state.alarm}, finalizing=${observed.state.finalizing !== null})`);
      return problems;
    }
    const base = baseline.state.finished!;
    if (observed.state.finished.result_code !== base.result_code) {
      // 裁定の保存前に入力が失われ、別の経路で確定しただけなら許容する。
      // 元の裁定を誰かに告知していた場合だけが違反。
      const baseResultLines = new Set(allLines(baseline).filter(l => l.startsWith('#')));
      const toldBase = allLines(observed).some(l => baseResultLines.has(l));
      problems.push(toldBase
        ? `告知と異なる裁定で確定した: ${observed.state.finished.result_code}`
        : `${INPUT_LOST_OTHER_RESULT}: ${observed.state.finished.result_code}`);
      return problems;
    }
    if (base.exported_at_ms != null && observed.state.finished.exported_at_ms == null) {
      problems.push('棋譜が export されていない');
    }
    if (observed.kifuMoves !== baseline.kifuMoves) problems.push(`棋譜の手数が違う: ${observed.kifuMoves}`);
    if (observed.historyObjects !== baseline.historyObjects) problems.push(`floodgate 履歴が ${observed.historyObjects} 件`);
    if (observed.liveEntries !== 0) problems.push(CRON_RECOVERED);
    for (const watcher of Object.keys(baseline.lines) as Watcher[]) {
      const got = observed.lines[watcher] ?? [];
      const want = baseline.lines[watcher] ?? [];
      if (!isSubsequence(got, want)) {
        problems.push(`${watcher} が重複・余計・順序違いの行を受信: ${JSON.stringify(got)}`);
      } else if (got.length < want.length && observed.closed[watcher] !== 1011) {
        // 送信に失敗した接続は 1011 で閉じる契約なので、欠落を許すのはその場合だけ。
        problems.push(`${watcher} が 1011 で閉じられていないのに行が欠けた (close=${observed.closed[watcher]}): ${JSON.stringify(got)}`);
      }
    }
    return problems;
  }

  for (const scenario of SCENARIOS) {
    for (const mode of MODES) {
      it(`${scenario.name} / ${mode}`, async () => {
        const baseline = await runCase(scenario, { at: 0, mode });
        expect(baseline.state.finished).not.toBeNull();
        expect(baseline.state.finished!.exported_at_ms).not.toBeNull();
        expect(baseline.state.finished!.exported_at_ms).not.toBeUndefined();
        expect(baseline.kifuMoves).toBe(scenario.beforeAgree ? 0 : scenario.name.startsWith('sennichite') ? 12 : 11);
        expect(baseline.historyObjects).toBe(1);
        const total = baseline.state.ops.length;
        for (let at = 1; at <= total; at++) {
          if (ONLY_CASE && ONLY_CASE !== `${scenario.name}/${mode}/${at}`) continue;
          const observed = await runCase(scenario, { at, mode });
          const op = observed.state.injectedAt?.name ?? `(未到達) ${baseline.state.ops[at - 1]}`;
          const problems = check(baseline, observed);
          if (ONLY_CASE) console.log(JSON.stringify({ baseline: baseline.lines, observed, problems }, null, 2));
          const finding = { scenario: scenario.name, mode, at, op };
          const unannounced = (problems.length === 1 && problems[0]!.startsWith('確定していない (') && !observed.state.finalizing)
            || (problems.length === 1 && problems[0]!.startsWith(INPUT_LOST_OTHER_RESULT));
          if (unannounced) {
            inputLost.push({ ...finding, problem: problems[0]! });
            continue;
          }
          for (const problem of problems) {
            (problem === CRON_RECOVERED ? cronRecovered : violations).push({ ...finding, problem });
          }
        }
        const mine = violations.filter(v => v.scenario === scenario.name && v.mode === mode);
        console.log(`${scenario.name}/${mode}: ${total} 地点, 違反 ${mine.length}`);
        for (const v of mine) console.log(`  #${v.at} ${v.op}: ${v.problem}`);
        expect(mine).toEqual([]);
      });
    }
  }
});
