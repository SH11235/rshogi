import { resolve } from 'node:path';
import { expect, it } from 'vitest';
import { createMiniflare, getKifuBucket, makeTempPersistRoot } from './harness.ts';

it('cron は確定済みの一覧項目だけを回収し、未確定の対局と棋譜を保持する', async () => {
  const persist = await makeTempPersistRoot();
  const mf = await createMiniflare({
    persistRoot: persist.path,
    scriptPath: resolve(import.meta.dirname, '../finalization_exhaustive/exhaustive-worker.mjs'),
  });
  try {
    const writer = await mf.getR2Bucket('KIFU_BUCKET') as unknown as {
      put(key: string, value: string): Promise<unknown>;
    };
    const bucket = await getKifuBucket(mf);
    const started = Date.now();
    const entries = ['finished', 'active', 'csa-without-meta'];
    const liveKey = (id: string) => `live-games-index/000-${id}.json`;
    const body = (id: string) => JSON.stringify({ game_id: id, started_at_ms: started });
    for (const id of entries) await writer.put(liveKey(id), body(id));
    const metaKey = 'kifu-by-id/finished.meta.json';
    await writer.put(metaKey, '{}');
    const csaKeys = ['kifu-by-id/finished.csa', 'kifu-by-id/csa-without-meta.csa'];
    for (const key of csaKeys) await writer.put(key, 'V2.2\n%TORYO\n');

    for (let sweep = 0; sweep < 2; sweep++) {
      const response = await mf.dispatchFetch('https://example.com/__test/cron', { method: 'POST' });
      expect(response.status).toBe(200);
      expect(await bucket.get(liveKey('finished'))).toBeNull();
      for (const id of ['active', 'csa-without-meta']) {
        expect(await (await bucket.get(liveKey(id)))?.text()).toBe(body(id));
      }
      expect(await (await bucket.get(metaKey))?.text()).toBe('{}');
      for (const key of csaKeys) expect(await (await bucket.get(key))?.text()).toBe('V2.2\n%TORYO\n');
    }
  } finally {
    await mf.dispose();
    await persist.cleanup();
  }
});
