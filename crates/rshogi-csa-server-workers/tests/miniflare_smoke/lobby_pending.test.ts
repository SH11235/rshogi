import { afterEach, beforeEach, describe, expect, test } from 'vitest';
import { resolve } from 'node:path';
import NodeWebSocket from 'ws';
import type { Miniflare, WebSocket } from 'miniflare';
import { createMiniflare, makeTempPersistRoot } from './harness';
import { readLineFromWebSocket } from './ws_test_helpers';

describe('Lobby 未認証接続の保持制限', () => {
  let mf: Miniflare;
  let cleanup: () => Promise<void>;
  let control: (command: object) => Promise<any>;
  let direct: (headers: Record<string, string>) => Promise<any>;
  async function upgrade(ip = '192.0.2.1') {
    return mf.dispatchFetch('https://example.com/ws/lobby', {
      headers: { Upgrade: 'websocket', 'CF-Connecting-IP': ip },
    });
  }
  async function connect(ip = '192.0.2.1'): Promise<WebSocket> {
    const response = await upgrade(ip);
    expect(response.status).toBe(101);
    response.webSocket!.accept();
    return response.webSocket!;
  }
  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanup = persist.cleanup;
    mf = await createMiniflare({ persistRoot: persist.path, privateChallengeEnabled: true,
      scriptPath: resolve(import.meta.dirname, 'lobby-pending-worker.mjs') });
    const ns = await mf.getDurableObjectNamespace('LOBBY') as any;
    const stub = ns.get(ns.idFromName('default'));
    direct = headers => stub.fetch('https://test/ws/lobby', { headers });
    control = async command => {
      const response = await stub.fetch('https://test/__test', { method: 'POST', body: JSON.stringify(command) });
      expect(response.status).toBe(200);
      return response.json();
    };
  });
  afterEach(async () => { await mf.dispose(); await cleanup(); });

  test('同一IPは4接続までで、異なるIPは受理される', async () => {
    await Promise.all(Array.from({ length: 4 }, () => connect()));
    const rejected = await upgrade();
    expect(rejected.status).toBe(503);
    expect(rejected.headers.get('Retry-After')).toBe('30');
    await connect('192.0.2.2');
  });

  test('全体100接続の上限はIPを分散しても適用される', async () => {
    for (let i = 0; i < 100; i++) await connect(`198.51.100.${i + 1}`);
    expect((await upgrade('203.0.113.1')).status).toBe(503);
  });

  test('未送信の接続も30秒後に自動切断される', async () => {
    const url = new URL('/ws/lobby', await mf.ready);
    url.protocol = 'ws:';
    // close frame 未受信ではタイマーは始まらず、無切断の回帰を隠さない。
    const ws = new NodeWebSocket(url, { headers: { 'CF-Connecting-IP': '192.0.2.1' }, closeTimeout: 1_000 });
    const closed = new Promise<number>(resolve => ws.once('close', code => resolve(code)));
    try {
      await new Promise<void>((resolve, reject) => { ws.once('open', resolve); ws.once('error', reject); });
      const state = await control({});
      const deadline = state.attachments[0].Pending.expires_at_ms;
      expect(deadline - Date.now()).toBeGreaterThan(25_000);
      expect(state.alarm).toBeGreaterThanOrEqual(deadline);
      expect(await closed).toBe(1008);
    } finally { if (ws.readyState !== NodeWebSocket.CLOSED) ws.terminate(); }
  }, 40_000);

  test('無効コマンド・binaryは期限を延長せず、復帰後も同じ期限を使う', async () => {
    const ws = await connect();
    const before = await control({});
    const lines = readLineFromWebSocket(ws);
    ws.send('CHALLENGE_LOBBY invalid\n');
    expect(await lines.takeLine()).toContain('CHALLENGE_LOBBY:incorrect');
    ws.send(new Uint8Array([1, 2, 3]));
    const after = await control({ reset: true });
    expect(after.attachments[0].Pending.expires_at_ms).toBe(before.attachments[0].Pending.expires_at_ms);
    await control({ expire: true, alarm: true });
    expect((await control({})).closes.some((c: any) => c.code === 1008)).toBe(true);
  });

  test('期限切れcleanupはclose完了を待たずslotを解放し、古い接続はLOGINできない', async () => {
    const sockets = await Promise.all(Array.from({ length: 4 }, () => connect()));
    await control({ holdClose: true, expire: true, alarm: true });
    const old = readLineFromWebSocket(sockets[0]);
    sockets[0].send('LOGIN_LOBBY stale+test+black pw\n');
    await expect(old.takeLine(200)).rejects.toThrow('timeout');
    await connect();
    expect((await control({})).closes.length).toBeGreaterThanOrEqual(4);
  });

  test('期限がない旧attachmentは復帰後fail-closed、上限へ永久に残らない', async () => {
    await Promise.all(Array.from({ length: 4 }, () => connect()));
    await control({ holdClose: true, legacy: true, reset: true, alarm: true });
    await connect();
    expect((await control({})).closes.length).toBeGreaterThanOrEqual(4);
  });

  test('DOに直接渡すIP欠落・空白はaccept前に拒否する', async () => {
    for (const headers of [{ Upgrade: 'websocket' }, { Upgrade: 'websocket', 'CF-Connecting-IP': ' ' }]) {
      expect((await direct(headers)).status).toBe(503);
    }
    expect((await control({})).attachments).toHaveLength(0);
  });

  async function waitForCloseRequest() {
    for (let attempt = 0; attempt < 100; attempt++) {
      const state = await control({});
      if (state.closes.length > 0) return state;
      await new Promise(resolve => setTimeout(resolve, 20));
    }
    throw new Error('server did not request close');
  }

  test('同じWSの並行LOGINは一つだけqueueへ遷移する', async () => {
    const ws = await connect();
    const lines = readLineFromWebSocket(ws);
    ws.send('LOGIN_LOBBY alice+test+black pw\n');
    ws.send('LOGIN_LOBBY bob+test+black pw\n');
    expect(await lines.takeLine()).toMatch(/^LOGIN_LOBBY:(alice|bob) OK$/);
    await expect(lines.takeLine(500)).rejects.toThrow('timeout');
    expect((await control({})).attachments.filter((att: any) => att.Queued)).toHaveLength(1);
  });

  test('challenge registry読み込み中の期限切れはtokenを発行しない', async () => {
    const ws = await connect();
    await control({ faults: { expireAfterRead: true } });
    ws.send('CHALLENGE_LOBBY alice bob black byoyomi-600-10\n');
    const state = await waitForCloseRequest();
    expect(Object.keys(state.registry?.entries ?? {})).toHaveLength(0);
    expect(state.faults.expireAfterRead).toBe(false);
  });

  test('private登録保存中の期限切れはunmarkされ同handleが再登録できる', async () => {
    const issuer = await connect();
    const replies = readLineFromWebSocket(issuer);
    issuer.send('CHALLENGE_LOBBY alice bob black byoyomi-600-10\n');
    const token = (await replies.takeLine()).split(' ')[1];
    await control({ faults: { expireAfterMark: true } });
    issuer.send(`LOGIN_LOBBY alice+private-${token}+free pw\n`);
    const state = await waitForCloseRequest();
    expect(state.faults.expireAfterMark).toBe(false);
    const retry = await connect();
    const retried = readLineFromWebSocket(retry);
    retry.send(`LOGIN_LOBBY alice+private-${token}+free pw\n`);
    expect(await retried.takeLine()).toContain('LOGIN_LOBBY:alice OK');
  });

  test('RateLimiterは並行消費・再構築の残量・読込失敗時のfail-closedを維持する', async () => {
    const ns = await mf.getDurableObjectNamespace('RATE_LIMITER') as any;
    const stub = ns.get(ns.idFromName('pending-rate-regression'));
    const consume = async () => (await stub.fetch('https://test/check?capacity=2', { method: 'POST' })).json();
    const setup = async (command: object) => stub.fetch('https://test/__test', { method: 'POST', body: JSON.stringify(command) });
    await setup({ reset: true, failRead: true });
    expect((await consume()).allowed).toBe(false);
    const responses = await Promise.all([consume(), consume(), consume()]);
    expect(responses.filter(response => response.allowed)).toHaveLength(2);
    await setup({ reset: true });
    expect((await consume()).allowed).toBe(false);
  });

  test('queuedとprivate認証後の接続は未認証期限のpurge対象にならない', async () => {
    const queued = await connect();
    const q = readLineFromWebSocket(queued);
    queued.send('LOGIN_LOBBY alice+test+black pw\n');
    expect(await q.takeLine()).toBe('LOGIN_LOBBY:alice OK');
    const inviter = await connect();
    const inv = readLineFromWebSocket(inviter);
    inviter.send('CHALLENGE_LOBBY carol dave black byoyomi-600-10\n');
    const challenge = await inv.takeLine();
    expect(challenge).toMatch(/^CHALLENGE_LOBBY:OK /);
    const invitationState = await control({});
    const pendingExpiry = invitationState.attachments.find((att: any) => att.Pending).Pending.expires_at_ms;
    expect(invitationState.alarm).toBeLessThanOrEqual(pendingExpiry + 1_000);
    const token = challenge.split(' ')[1];
    inviter.send(`LOGIN_LOBBY carol+private-${token}+free pw\n`);
    expect(await inv.takeLine()).toContain('LOGIN_LOBBY:carol OK');
    const pending = await connect();
    const state = await control({ expire: true, alarm: true });
    expect(state.attachments.some((att: any) => att.Queued)).toBe(true);
    expect(state.attachments.some((att: any) => att.PrivatePending)).toBe(true);
    expect(state.alarm).toBeGreaterThan(Date.now());
    queued.send('LOBBY_PONG\n');
    await expect(q.takeLine(200)).rejects.toThrow('timeout');
    pending.close();
  });
});


test('Lobby upgrade bucketはLOGINとGameRoom upgradeのbucketを消費しない', async () => {
  const persist = await makeTempPersistRoot();
  const mf = await createMiniflare({ persistRoot: persist.path,
    rateLimitOverrides: { wsRoomUpgradePerIpPerMin: 2, lobbyLoginPerIpPerMin: 1 } });
  const headers = { Upgrade: 'websocket', 'CF-Connecting-IP': '192.0.2.40' };
  try {
    const first = await mf.dispatchFetch('https://example.com/ws/lobby', { headers });
    expect(first.status).toBe(101);
    first.webSocket!.accept();
    const lines = readLineFromWebSocket(first.webSocket!);
    first.webSocket!.send('LOGIN_LOBBY alice+test+black pw\n');
    expect(await lines.takeLine()).toBe('LOGIN_LOBBY:alice OK');
    const second = await mf.dispatchFetch('https://example.com/ws/lobby', { headers });
    expect(second.status).toBe(101);
    second.webSocket!.accept();
    const rejected = await mf.dispatchFetch('https://example.com/ws/lobby', { headers });
    expect(rejected.status).toBe(503);
    expect(Number(rejected.headers.get('Retry-After'))).toBeGreaterThan(0);
    const room = await mf.dispatchFetch('https://example.com/ws/pending-test-room', { headers });
    expect(room.status).toBe(101);
    room.webSocket!.accept();
  } finally { await mf.dispose(); await persist.cleanup(); }
});
