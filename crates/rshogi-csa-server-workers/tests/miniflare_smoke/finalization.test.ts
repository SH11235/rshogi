import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { resolve } from 'node:path';
import type { Miniflare } from 'miniflare';
import { CsaClient, createMiniflare, makeTempPersistRoot, getKifuBucket } from './harness.ts';

interface RecoveryState {
  finished: { result_code: string; exported_at_ms: number | null } | null;
  kind: string | null;
  pending: unknown;
  alarm: number | null;
  moves: number;
}

describe('終局保存の復旧', () => {
  let mf: Miniflare;
  let cleanup: () => Promise<void>;
  let black: CsaClient;
  let white: CsaClient;
  let control: (command: object) => Promise<RecoveryState>;
  const cycle = ['+5958OU', '-5152OU', '+5859OU', '-5251OU'] as const;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanup = persist.cleanup;
    mf = await createMiniflare({ persistRoot: persist.path,
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
    await black.recvUntil(l => l.startsWith('START:'));
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
  ])('送信失敗後に未配信分だけを送る ($role / $line / cold=$cold)', async ({ cold, role, line }) => {
    await control({ faults: { sendRole: role, sendLine: line } });
    white.send(cycle[3]);
    await black.recvUntil(l => l.startsWith(cycle[3]));
    await white.recvUntil(l => l.startsWith(cycle[3]));
    const failed = role === 'Black' ? black : white;
    const successful = role === 'Black' ? white : black;
    expect(await successful.recvLine()).toBe('#SENNICHITE');
    expect(await successful.recvLine()).toBe('#DRAW');
    if (line === '#DRAW') expect(await failed.recvLine()).toBe('#SENNICHITE');
    const pending = await control({});
    expect(pending.finished).toBeNull();
    await control({ faults: {}, reset: cold, alarm: true });
    if (line === '#SENNICHITE') expect(await failed.recvLine()).toBe('#SENNICHITE');
    expect(await failed.recvLine()).toBe('#DRAW');
    await expect(black.recvLine()).rejects.toThrow('connection closed');
    await expect(white.recvLine()).rejects.toThrow('connection closed');
  });

  it('終局手保存直後の中断から両接続へ結果通知を回復する', async () => {
    await control({ faults: { afterMove: true } });
    white.send(cycle[3]);
    await waitForMove();
    await control({ faults: {}, reset: true, alarm: true });
    for (const client of [black, white]) {
      expect(await client.recvLine()).toBe('#SENNICHITE');
      expect(await client.recvLine()).toBe('#DRAW');
      await expect(client.recvLine()).rejects.toThrow('connection closed');
    }
  });
});
