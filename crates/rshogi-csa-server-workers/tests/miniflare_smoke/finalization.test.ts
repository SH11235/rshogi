import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { resolve } from 'node:path';
import type { Miniflare } from 'miniflare';
import { CsaClient, createMiniflare, makeTempPersistRoot, getKifuBucket, getFloodgateHistoryBucket } from './harness.ts';
import { readLineFromWebSocket } from './ws_test_helpers';

interface RecoveryState {
  activeMessages: number;
  finalizing: { attempt: number; broadcasts: { line: string; ply: number | null }[] } | null;
  closes: { role: string; code: number; reason: string }[];
  finished: { result_code: string; exported_at_ms: number | null } | null;
  kind: string | null;
  pending: unknown;
  alarm: number | null;
  moves: number;
  beforeMoveArmed: boolean;
  puts: Record<string, number>;
}

describe('終局保存の復旧', () => {
  let mf: Miniflare;
  let cleanup: () => Promise<void>;
  let black: CsaClient;
  let white: CsaClient;
  let gameId: string;
  let control: (command: object) => Promise<RecoveryState>;
  const cycle = ['+5958OU', '-5152OU', '+5859OU', '-5251OU'] as const;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanup = persist.cleanup;
    mf = await createMiniflare({ persistRoot: persist.path, allowViewerApi: true, allowFloodgateFeatures: true,
      scriptPath: resolve(import.meta.dirname, 'finalization-worker.mjs') });
    const ns = await mf.getDurableObjectNamespace('GAME_ROOM') as unknown as {
      idFromName(name: string): unknown;
      get(id: unknown): { fetch(url: string, init: RequestInit): Promise<Response> };
    };
    const stub = ns.get(ns.idFromName('finalization'));
    control = async (command) => {
      const response = await stub.fetch('https://test/__test', { method: 'POST', body: JSON.stringify(command) });
      if (!response.ok) throw new Error(await response.text());
      return await response.json() as RecoveryState;
    };
    black = await CsaClient.connect(mf, 'finalization');
    black.send('LOGIN black+test+black pw');
    expect(await black.recvLine()).toBe('LOGIN:black+test+black OK');
    white = await CsaClient.connect(mf, 'finalization');
    white.send('LOGIN white+test+white pw');
    expect(await white.recvLine()).toBe('LOGIN:white+test+white OK');
    await black.drainGameSummary();
    await white.drainGameSummary();
    black.send('AGREE'); white.send('AGREE');
    gameId = (await black.recvUntil(l => l.startsWith('START:'))).at(-1)!.slice('START:'.length);
    await white.recvUntil(l => l.startsWith('START:'));
    for (let i = 0; i < 11; i++) {
      const line = cycle[i % 4]!;
      (i % 2 === 0 ? black : white).send(line);
      await black.recvUntil(l => l.startsWith(line));
      await white.recvUntil(l => l.startsWith(line));
    }
  });
  afterEach(async () => { await mf?.dispose(); await cleanup?.(); });

  async function waitForMove() {
    for (let i = 0; i < 100; i++) {
      if ((await control({})).moves === 12) return;
      await new Promise(r => setTimeout(r, 10));
    }
    throw new Error('終局手が保存されなかった');
  }

  async function waitForFinished() {
    for (let i = 0; i < 100; i++) {
      const state = await control({});
      if (state.finished) return state;
      await new Promise(r => setTimeout(r, 10));
    }
    throw new Error('終局が確定しなかった');
  }

  it('grace 復元中の R2 失敗後も export alarm が棋譜保存を再試行する', async () => {
    await control({ faults: { afterMove: true, r2: true }, grace: true });
    white.send(cycle[3]);
    await waitForMove();
    const failed = await control({ reset: true, alarm: true });
    expect(failed.finished?.result_code).toBe('#SENNICHITE');
    expect(failed.kind).toBe('ExportRetry');
    expect(failed.pending).not.toBeNull();
    expect(failed.alarm).not.toBeNull();
    const recovered = await control({ faults: {}, alarm: true });
    expect(recovered.finished?.exported_at_ms).toEqual(expect.any(Number));
    expect(recovered.pending).toBeNull();
    const bucket = await getKifuBucket(mf);
    const keys = await bucket.list();
    expect(keys.objects.some(o => o.key.endsWith('.csa'))).toBe(true);
  });

  it.each([
    { cold: false, role: 'Black', line: '#SENNICHITE' },
    { cold: false, role: 'Black', line: '#DRAW' },
    { cold: false, role: 'White', line: '#SENNICHITE' },
    { cold: false, role: 'White', line: '#DRAW' },
    { cold: true, role: 'Black', line: '#SENNICHITE' },
    { cold: true, role: 'Black', line: '#DRAW' },
    { cold: true, role: 'White', line: '#SENNICHITE' },
    { cold: true, role: 'White', line: '#DRAW' },
  ])('送信失敗した接続は再送せず 1011 で閉じる ($role / $line / cold=$cold)', async ({ cold, role, line }) => {
    await control({ faults: { sendRole: role, sendLine: line } });
    white.send(cycle[3]);
    await black.recvUntil(l => l.startsWith(cycle[3]));
    await white.recvUntil(l => l.startsWith(cycle[3]));
    const failed = role === 'Black' ? black : white;
    const successful = role === 'Black' ? white : black;
    expect(await successful.recvLine()).toBe('#SENNICHITE');
    expect(await successful.recvLine()).toBe('#DRAW');
    if (line === '#DRAW') expect(await failed.recvLine()).toBe('#SENNICHITE');
    const finished = await waitForFinished();
    expect(finished.finished?.result_code).toBe('#SENNICHITE');
    expect(finished.closes).toContainEqual({ role, code: 1011, reason: 'terminal delivery failed' });
    await control({ faults: {}, reset: cold, alarm: true });
    await expect(black.recvLine()).rejects.toThrow('connection closed');
    await expect(white.recvLine()).rejects.toThrow('connection closed');
  });

  it.each(['#SENNICHITE', '#DRAW'])('進捗保存の失敗後も通知済みの行を再送しない (%s)', async (line) => {
    await control({ faults: { persistRole: 'White', persistLine: line } });
    white.send(cycle[3]);
    await black.recvUntil(l => l.startsWith(cycle[3]));
    await white.recvUntil(l => l.startsWith(cycle[3]));
    expect(await black.recvLine()).toBe('#SENNICHITE');
    expect(await black.recvLine()).toBe('#DRAW');
    expect(await white.recvLine()).toBe('#SENNICHITE');
    if (line === '#DRAW') expect(await white.recvLine()).toBe('#DRAW');
    expect((await control({})).finished).toBeNull();
    const recovered = await control({ alarm: true });
    expect(recovered.closes).toContainEqual({ role: 'White', code: 1011, reason: 'terminal delivery failed' });
    await expect(white.recvLine()).rejects.toThrow('connection closed');
  });

  it.each([false, true])('終局手を保存できなければ終局を確定せず保存済み局面へ戻す (cold=%s)', async (cold) => {
    await control({ faults: { beforeMove: true } });
    white.send(cycle[3]);
    for (let i = 0; i < 100 && (await control({})).beforeMoveArmed; i++) {
      await new Promise(r => setTimeout(r, 10));
    }
    const reverted = await control({ reset: cold, alarm: true });
    expect(reverted.finished).toBeNull();
    expect(reverted.moves).toBe(11);
    white.send(cycle[3]);
    for (const client of [black, white]) {
      expect(await client.recvLine()).toMatch(new RegExp(`^\\${cycle[3]},T`));
      expect(await client.recvLine()).toBe('#SENNICHITE');
      expect(await client.recvLine()).toBe('#DRAW');
    }
    expect((await control({})).finished?.result_code).toBe('#SENNICHITE');
    const bucket = await getKifuBucket(mf);
    const key = (await bucket.list()).objects.find(o => o.key.endsWith('.csa'))!.key;
    const moves = (await (await bucket.get(key))!.text()).split('\n').filter(l => /^[+-]\d{4}/.test(l));
    expect(moves).toHaveLength(12);
  });

  it('棋譜保存の待機中に切断が届いても終局確定を二重に実行しない', async () => {
    await control({ faults: { holdR2: true } });
    white.send(cycle[3]);
    for (const client of [black, white]) {
      await client.recvUntil(l => l === '#DRAW');
    }
    for (let i = 0; i < 100 && Object.keys((await control({})).puts).length === 0; i++) {
      await new Promise(r => setTimeout(r, 10));
    }
    const reentered = await control({ alarm: true });
    expect(reentered.finished).toBeNull();
    expect(reentered.alarm).not.toBeNull();
    await black.close();
    await new Promise(r => setTimeout(r, 100));
    await control({ releaseR2: true });
    let state = await control({});
    for (let i = 0; i < 100 && !state.finished; i++) {
      await new Promise(r => setTimeout(r, 10));
      state = await control({});
    }
    expect(state.finished?.result_code).toBe('#SENNICHITE');
    expect(Object.values(state.puts).every(n => n === 1)).toBe(true);
  });

  it('盤面を進めない終局は isolate 破棄後も保存した裁定で確定する', async () => {
    await control({ faults: { persistRole: 'Black', persistLine: '#RESIGN' } });
    white.send('%TORYO');
    await white.recvUntil(l => l === '#LOSE');
    expect((await control({})).finished).toBeNull();
    const state = await control({ faults: {}, reset: true, alarm: true });
    expect(state.finished?.result_code).toBe('#RESIGN');
    expect(await black.recvLine()).toBe('#RESIGN');
    expect(state.closes).toContainEqual({ role: 'Black', code: 1011, reason: 'terminal delivery failed' });
    await expect(black.recvLine()).rejects.toThrow('connection closed');
  });

  it('保存済み裁定を AgreeTimeout より優先し、履歴読込失敗でも上書きしない', async () => {
    await control({ faults: { persistRole: 'White', persistLine: '#DRAW' } });
    white.send(cycle[3]);
    await black.recvUntil(l => l === '#DRAW');
    await white.recvUntil(l => l === '#SENNICHITE');
    await control({ faults: { loadMoves: true }, agreeTimeout: true });
    await control({ reset: true, alarm: true });
    const pending = await control({});
    expect(pending.finished).toBeNull();
    expect(pending.finalizing?.attempt).toBe(2);
    const broadcasts = pending.finalizing!.broadcasts;
    const moveIndex = broadcasts.findIndex(entry => /^-5251OU,T\d+$/.test(entry.line));
    expect(moveIndex).toBeGreaterThanOrEqual(0);
    expect(broadcasts[moveIndex + 1]!.line).toMatch(/^##\[CLOCK\]/);
    expect(broadcasts[moveIndex + 1]!.ply ?? null).toBeNull();
    expect(pending.alarm).not.toBeNull();
    const finished = await control({ faults: {}, reset: true, alarm: true });
    expect(finished.finished?.result_code).toBe('#SENNICHITE');
    expect(await white.recvLine()).toBe('#DRAW');
    expect(finished.closes).toContainEqual({ role: 'White', code: 1011, reason: 'terminal delivery failed' });
    await expect(white.recvLine()).rejects.toThrow('connection closed');
  });

  it('floodgate 履歴保存失敗では確定を保留し、同じキーで再試行する', async () => {
    await control({ faults: { history: true } });
    white.send(cycle[3]);
    await black.recvUntil(l => l === '#DRAW');
    await white.recvUntil(l => l === '#DRAW');
    let pending = await control({});
    for (let i = 0; i < 100 && pending.activeMessages > 0; i++) {
      await new Promise(r => setTimeout(r, 10));
      pending = await control({});
    }
    expect(pending.activeMessages).toBe(0);
    expect(pending.finished).toBeNull();
    expect(pending.finalizing?.attempt).toBe(1);
    expect(pending.alarm).not.toBeNull();
    const retried = await control({ reset: true, alarm: true });
    expect(retried.finished).toBeNull();
    expect(retried.alarm).not.toBeNull();
    const finished = await control({ faults: {}, reset: true, alarm: true });
    expect(finished.finished?.result_code).toBe('#SENNICHITE');
    const history = await getFloodgateHistoryBucket(mf);
    expect((await history.list()).objects).toHaveLength(1);
    await expect(white.recvLine()).rejects.toThrow('connection closed');
  });

  it('履歴保存が失敗し続けても初回と 5 回の alarm 試行で確定する', async () => {
    const faults = { history: true };
    await control({ faults });
    white.send(cycle[3]);
    await black.recvUntil(l => l === '#DRAW');
    await white.recvUntil(l => l === '#DRAW');
    for (let attempt = 2; attempt <= 5; attempt++) {
      await control({ faults, reset: true, alarm: true });
      const state = await control({});
      expect(state.finalizing?.attempt).toBe(attempt);
      expect(state.finished).toBeNull();
      expect(state.alarm).not.toBeNull();
    }
    const state = await control({ faults, reset: true, alarm: true });
    expect(state.finished?.result_code).toBe('#SENNICHITE');
    await expect(white.recvLine()).rejects.toThrow('connection closed');
  });

  it('観戦 snapshot の後に終局通知を送る', async () => {
    await control({ faults: { afterMove: true } });
    white.send(cycle[3]);
    await waitForMove();
    await control({ faults: {}, reset: true });
    const res = await mf.dispatchFetch(`https://example.com/ws/${encodeURIComponent(gameId)}/spectate`, {
      headers: { Upgrade: 'websocket', Origin: 'https://example.com', 'CF-Connecting-IP': '127.0.0.1' },
    });
    const ws = res.webSocket!;
    const buf = readLineFromWebSocket(ws);
    ws.accept();
    ws.send(`%%MONITOR2ON ${gameId}\n`);
    const lines: string[] = [];
    while (lines.at(-1) !== '#DRAW') lines.push(await buf.takeLine(5000));
    const end = lines.indexOf('##[MONITOR2] END');
    expect(lines[0]).toBe(`##[MONITOR2] BEGIN ${gameId}`);
    expect(lines.slice(0, end).some(l => l.startsWith(cycle[3]))).toBe(true);
    expect(lines[end + 1]).toMatch(/^##\[CLOCK\]/);
    expect(lines.slice(end + 2)).toEqual(['#SENNICHITE', '#DRAW']);
    await expect(buf.takeLine(5000)).rejects.toThrow('connection closed');
    for (const client of [black, white]) {
      expect(await client.recvLine()).toMatch(/^-5251OU,T\d+$/);
      expect(await client.recvLine()).toBe('#SENNICHITE');
      expect(await client.recvLine()).toBe('#DRAW');
    }
  });

  it('終局手保存直後の中断から両接続へ結果通知を回復する', async () => {
    await control({ faults: { afterMove: true } });
    white.send(cycle[3]);
    await waitForMove();
    await control({ faults: {}, reset: true, alarm: true });
    for (const client of [black, white]) {
      expect(await client.recvLine()).toMatch(/^-5251OU,T\d+$/);
      expect(await client.recvLine()).toBe('#SENNICHITE');
      expect(await client.recvLine()).toBe('#DRAW');
      await expect(client.recvLine()).rejects.toThrow('connection closed');
    }
  });
});
