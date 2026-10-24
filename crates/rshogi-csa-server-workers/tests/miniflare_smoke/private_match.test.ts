import { afterEach, beforeEach, describe, expect, test } from 'vitest';
import { resolve } from 'node:path';
import { createHash } from 'node:crypto';
import type { Miniflare } from 'miniflare';
import { CsaClient, createMiniflare, makeTempPersistRoot } from './harness';
import { readLineFromWebSocket } from './ws_test_helpers';

describe('private match dispatch', () => {
  let mf: Miniflare;
  let cleanup: () => Promise<void>;
  let nextIp: number;
  const roomId = (token: string) => `private-${createHash('sha256').update(token).digest('hex')}`;
  async function connectLobby() {
    const response = await mf.dispatchFetch('https://example.com/ws/lobby', {
      headers: { Upgrade: 'websocket', 'CF-Connecting-IP': `192.0.2.${nextIp++}` },
    });
    expect(response.status).toBe(101);
    const ws = response.webSocket!;
    const lines = readLineFromWebSocket(ws);
    ws.accept();
    return { ws, lines };
  }
  async function control(binding: string, id: string, command: object) {
    const ns = await mf.getDurableObjectNamespace(binding) as any;
    const response = await ns.get(ns.idFromName(id)).fetch('https://test/__test', {
      method: 'POST', body: JSON.stringify(command),
    });
    expect(response.status).toBe(200);
    return response.json();
  }
  async function issue(color = 'black', sfen = '') {
    const issuer = await connectLobby();
    issuer.ws.send(`CHALLENGE_LOBBY alice bob ${color} byoyomi-600-10${sfen ? ` ${sfen}` : ''}\n`);
    const reply = await issuer.lines.takeLine();
    expect(reply).toMatch(/^CHALLENGE_LOBBY:OK [0-9a-f]{24} 3600$/);
    return { issuer, token: reply.split(' ')[1] };
  }
  async function loginLobby(client: Awaited<ReturnType<typeof connectLobby>>, handle: string, token: string) {
    client.ws.send(`LOGIN_LOBBY ${handle}+private-${token}+free pw\n`);
    expect(await client.lines.takeLine()).toBe(`LOGIN_LOBBY:${handle} OK`);
  }
  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanup = persist.cleanup;
    nextIp = 1;
    mf = await createMiniflare({ persistRoot: persist.path, privateChallengeEnabled: true,
      scriptPath: resolve(import.meta.dirname, 'lobby-pending-worker.mjs') });
  });
  afterEach(async () => { await mf.dispose(); await cleanup(); });

  test.each(['black', 'white', 'free'])('両LOGINで一度だけ成立し指定配色・時計・SFENから終局する: %s', async color => {
    const sfen = 'lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL w - 2';
    const { issuer, token } = await issue(color, sfen);
    const opponent = await connectLobby();
    await Promise.all([loginLobby(issuer, 'alice', token), loginLobby(opponent, 'bob', token)]);
    const a = (await issuer.lines.takeLine()).split(' ');
    const b = (await opponent.lines.takeLine()).split(' ');
    expect(a[0]).toBe('MATCHED'); expect(b[0]).toBe('MATCHED');
    expect(a[1]).toBe(roomId(token)); expect(b[1]).toBe(a[1]);
    expect(new Set([a[2], b[2]])).toEqual(new Set(['black', 'white']));
    if (color !== 'free') expect(a[2]).toBe(color);
    const replay = await connectLobby();
    replay.ws.send(`LOGIN_LOBBY alice+private-${token}+free pw\n`);
    expect(await replay.lines.takeLine()).toBe('LOGIN_LOBBY:incorrect challenge_expired');
    await control('GAME_ROOM', a[1], { reset: true });
    const alice = await CsaClient.connect(mf, a[1], 'https://example.com', `198.51.100.${nextIp++}`);
    alice.send(`LOGIN alice+private-${token}+${a[2]} pw`);
    expect(await alice.recvLine()).toBe(`LOGIN:alice+private-${token}+${a[2]} OK`);
    const bob = await CsaClient.connect(mf, a[1], 'https://example.com', `198.51.100.${nextIp++}`);
    bob.send(`LOGIN bob+private-${token}+${b[2]} pw`);
    expect(await bob.recvLine()).toBe(`LOGIN:bob+private-${token}+${b[2]} OK`);
    for (const summary of [await alice.drainGameSummary(), await bob.drainGameSummary()]) {
      expect(summary).toContain('To_Move:-');
      expect(summary).toContain('Total_Time:600');
      expect(summary).toContain('Byoyomi:10');
      expect(summary.join('\n')).not.toContain(token);
      expect(summary).toContain(`Name+:${a[2] === 'black' ? 'alice' : 'bob'}`);
      expect(summary).toContain(`Name-:${a[2] === 'white' ? 'alice' : 'bob'}`);
    }
    expect(JSON.stringify(await control('GAME_ROOM', a[1], {}))).not.toContain(token);
    alice.send('AGREE'); bob.send('AGREE');
    await alice.recvUntil(line => line.startsWith('START:'));
    await bob.recvUntil(line => line.startsWith('START:'));
    const white = a[2] === 'white' ? alice : bob;
    const black = a[2] === 'black' ? alice : bob;
    white.send('%TORYO');
    expect(await white.recvUntil(line => line === '#LOSE')).toContain('#RESIGN');
    expect(await black.recvUntil(line => line === '#WIN')).toContain('#RESIGN');
  });

  test('私的対局は招待期限後のcold startでも元のLOGIN名で再接続して継続できる', async () => {
    await mf.dispose(); await cleanup();
    const persist = await makeTempPersistRoot(); cleanup = persist.cleanup;
    mf = await createMiniflare({ persistRoot: persist.path, privateChallengeEnabled: true,
      reconnectGraceSeconds: 30, allowFloodgateFeatures: true,
      scriptPath: resolve(import.meta.dirname, 'lobby-pending-worker.mjs') });
    const { issuer, token } = await issue();
    const opponent = await connectLobby();
    await loginLobby(issuer, 'alice', token); await loginLobby(opponent, 'bob', token);
    expect(await issuer.lines.takeLine()).toBe(`MATCHED ${roomId(token)} black`);
    expect(await opponent.lines.takeLine()).toBe(`MATCHED ${roomId(token)} white`);
    const blackName = `alice+private-${token}+black`;
    const whiteName = `bob+private-${token}+white`;
    const black = await CsaClient.connect(mf, roomId(token), 'https://example.com', `198.51.100.${nextIp++}`);
    black.send(`LOGIN ${blackName} pw`);
    expect(await black.recvLine()).toBe(`LOGIN:${blackName} OK`);
    const white = await CsaClient.connect(mf, roomId(token), 'https://example.com', `198.51.100.${nextIp++}`);
    white.send(`LOGIN ${whiteName} pw`);
    expect(await white.recvLine()).toBe(`LOGIN:${whiteName} OK`);
    const summary = await black.drainGameSummary();
    await white.drainGameSummary();
    const reconnectToken = summary.find(line => line.startsWith('Reconnect_Token:'))?.slice('Reconnect_Token:'.length);
    expect(reconnectToken).toBeDefined();
    black.send('AGREE'); white.send('AGREE');
    const start = await black.recvLine();
    expect(start).toMatch(/^START:/);
    expect(await white.recvLine()).toBe(start);
    const gameId = start.slice('START:'.length);
    black.send('+7776FU');
    await black.recvUntil(line => line.startsWith('+7776FU'));
    await white.recvUntil(line => line.startsWith('+7776FU'));
    await black.close();
    await control('GAME_ROOM', roomId(token), { expirePrivate: true, reset: true });
    const resumed = await CsaClient.connect(mf, roomId(token), 'https://example.com', `198.51.100.${nextIp++}`);
    resumed.send(`LOGIN ${blackName} pw reconnect:${gameId}+${reconnectToken}`);
    expect(await resumed.recvLine()).toBe(`LOGIN:${blackName} OK`);
    const resumedSummary = await resumed.drainGameSummary();
    expect(resumedSummary).toContain('Total_Time:600');
    expect(resumedSummary).toContain('Byoyomi:10');
    expect(resumedSummary).toContain(`Reconnect_Token:${reconnectToken}`);
    expect(resumedSummary.join('\n')).not.toContain(token);
    expect(await resumed.recvLine()).toBe('BEGIN Reconnect_State');
    const state = await resumed.recvUntil(line => line === 'END Reconnect_State');
    expect(state).toContain('Current_Turn:-');
    for (const field of ['Black_Time_Remaining_Ms', 'White_Time_Remaining_Ms', 'Current_Turn_Remaining_Ms']) {
      const remaining = Number(state.find(line => line.startsWith(`${field}:`))?.split(':')[1]);
      expect(remaining).toBeGreaterThan(0);
      expect(remaining).toBeLessThanOrEqual(611_000);
    }
    white.send('-3334FU');
    await white.recvUntil(line => line.startsWith('-3334FU'));
    await resumed.recvUntil(line => line.startsWith('-3334FU'));
    resumed.send('%TORYO');
    expect(await resumed.recvUntil(line => line === '#LOSE')).toContain('#RESIGN');
    expect(await white.recvUntil(line => line === '#WIN')).toContain('#RESIGN');
  });

  test('招待外handle・色・token・予約なしprivate部屋は拒否し正しい参加を妨げない', async () => {
    const { issuer, token } = await issue();
    const stranger = await connectLobby();
    stranger.ws.send(`LOGIN_LOBBY mallory+private-${token}+free pw\n`);
    expect(await stranger.lines.takeLine()).toBe('LOGIN_LOBBY:incorrect not_invited');
    const wrongToken = await connectLobby();
    wrongToken.ws.send('LOGIN_LOBBY alice+private-ffffffffffffffffffffffff+free pw\n');
    expect(await wrongToken.lines.takeLine()).toBe('LOGIN_LOBBY:incorrect challenge_expired');
    const opponent = await connectLobby();
    await loginLobby(issuer, 'alice', token); await loginLobby(opponent, 'bob', token);
    await issuer.lines.takeLine(); await opponent.lines.takeLine();
    for (const name of [`mallory+private-${token}+black`, `alice+private-${token}+white`,
      'alice+private-ffffffffffffffffffffffff+black', `alice+${roomId(token)}+black`]) {
      const client = await CsaClient.connect(mf, roomId(token), 'https://example.com', `198.51.100.${nextIp++}`);
      client.send(`LOGIN ${name} pw`);
      expect(await client.recvLine()).toBe('LOGIN:incorrect');
    }
    const missing = await CsaClient.connect(mf, 'private-unprepared', 'https://example.com', `198.51.100.${nextIp++}`);
    missing.send(`LOGIN alice+private-${token}+black pw`);
    expect(await missing.recvLine()).toBe('LOGIN:incorrect');
    const accepted = await CsaClient.connect(mf, roomId(token), 'https://example.com', `198.51.100.${nextIp++}`);
    accepted.send(`LOGIN alice+private-${token}+black pw`);
    expect(await accepted.recvLine()).toContain(' OK');
  });

  test('prepare失敗は両待機者を解放し同tokenで再試行できる', async () => {
    const { issuer, token } = await issue();
    await control('GAME_ROOM', roomId(token), { failPrepare: true });
    const opponent = await connectLobby();
    await loginLobby(issuer, 'alice', token); await loginLobby(opponent, 'bob', token);
    expect(await issuer.lines.takeLine()).toBe('LOGIN_LOBBY:incorrect match_start_failed');
    expect(await opponent.lines.takeLine()).toBe('LOGIN_LOBBY:incorrect match_start_failed');
    await control('LOBBY', 'default', { reset: true });
    const retryA = await connectLobby(), retryB = await connectLobby();
    await loginLobby(retryA, 'alice', token); await loginLobby(retryB, 'bob', token);
    expect(await retryA.lines.takeLine()).toBe(`MATCHED ${roomId(token)} black`);
    expect(await retryB.lines.takeLine()).toBe(`MATCHED ${roomId(token)} white`);
  });

  test('切断後の再LOGINとcold startで登録を復元する', async () => {
    const { issuer, token } = await issue();
    await loginLobby(issuer, 'alice', token);
    const closed = new Promise<void>(resolve => issuer.ws.addEventListener('close', () => resolve(), { once: true }));
    issuer.ws.close(); await closed;
    await control('LOBBY', 'default', { reset: true });
    const retry = await connectLobby(); await loginLobby(retry, 'alice', token);
    await control('LOBBY', 'default', { reset: true });
    const opponent = await connectLobby(); await loginLobby(opponent, 'bob', token);
    expect(await retry.lines.takeLine()).toBe(`MATCHED ${roomId(token)} black`);
    expect(await opponent.lines.takeLine()).toBe(`MATCHED ${roomId(token)} white`);
  });

  test('self challenge・期限切れ・free以外を拒否する', async () => {
    const self = await connectLobby();
    self.ws.send('CHALLENGE_LOBBY alice alice black byoyomi-600-10\n');
    expect(await self.lines.takeLine()).toBe('CHALLENGE_LOBBY:incorrect self_challenge');
    const { issuer, token } = await issue();
    const invalid = await connectLobby();
    invalid.ws.send(`LOGIN_LOBBY alice+private-${token}+black pw\n`);
    expect(await invalid.lines.takeLine()).toContain('LOGIN_LOBBY:incorrect');
    await control('LOBBY', 'default', { expireChallenges: true, reset: true });
    issuer.ws.send(`LOGIN_LOBBY alice+private-${token}+free pw\n`);
    expect(await issuer.lines.takeLine()).toBe('LOGIN_LOBBY:incorrect challenge_expired');
  });

  test.each(['activate', 'notification'])('成立後の失敗はtokenを再使用せず部屋を取り消す: %s', async failure => {
    const { issuer, token } = await issue();
    if (failure === 'activate') await control('GAME_ROOM', roomId(token), { failActivate: true });
    else await control('LOBBY', 'default', { failMatchedHandle: 'bob' });
    const opponent = await connectLobby();
    await loginLobby(issuer, 'alice', token); await loginLobby(opponent, 'bob', token);
    if (failure === 'activate') {
      expect(await issuer.lines.takeLine()).toBe('LOGIN_LOBBY:incorrect challenge_expired');
      expect(await opponent.lines.takeLine()).toBe('LOGIN_LOBBY:incorrect challenge_expired');
    } else {
      expect(await issuer.lines.takeLine()).toBe(`MATCHED ${roomId(token)} black`);
      await expect(opponent.lines.takeLine()).rejects.toThrow('connection closed');
    }
    const retry = await connectLobby();
    retry.ws.send(`LOGIN_LOBBY alice+private-${token}+free pw\n`);
    expect(await retry.lines.takeLine()).toBe('LOGIN_LOBBY:incorrect challenge_expired');
    await expect.poll(async () => (await control('GAME_ROOM', roomId(token), {})).cancelled).toBe(true);
    await control('GAME_ROOM', roomId(token), { reset: true });
    for (const game of [`private-${token}`, 'test']) {
      const client = await CsaClient.connect(mf, roomId(token), 'https://example.com', `198.51.100.${nextIp++}`);
      client.send(`LOGIN alice+${game}+black pw`);
      expect(await client.recvLine()).toBe('LOGIN:incorrect');
    }
  });

  test('予約期限切れの部屋は通常対局名でも受け付けない', async () => {
    const { issuer, token } = await issue();
    const opponent = await connectLobby();
    await loginLobby(issuer, 'alice', token); await loginLobby(opponent, 'bob', token);
    await issuer.lines.takeLine(); await opponent.lines.takeLine();
    await control('GAME_ROOM', roomId(token), { expirePrivate: true, reset: true });
    for (const game of [`private-${token}`, 'test']) {
      const client = await CsaClient.connect(mf, roomId(token), 'https://example.com', `198.51.100.${nextIp++}`);
      client.send(`LOGIN alice+${game}+black pw`);
      expect(await client.recvLine()).toBe('LOGIN:incorrect');
    }
  });

  test('staleなsession idのpurgeで別接続を切断しない', async () => {
    const { issuer, token } = await issue();
    await loginLobby(issuer, 'alice', token);
    const state = await control('LOBBY', 'default', { stalePrivateMarks: true, expireChallenges: true, alarm: true });
    expect(state.closes).toEqual([]);
    issuer.ws.send('LOBBY_PONG\n');
    await expect(issuer.lines.takeLine(150)).rejects.toThrow('timeout');
  });

  test('feature gate無効では私的招待とLOGINを受け付けない', async () => {
    await mf.dispose(); await cleanup();
    const persist = await makeTempPersistRoot(); cleanup = persist.cleanup;
    mf = await createMiniflare({ persistRoot: persist.path, privateChallengeEnabled: false,
      scriptPath: resolve(import.meta.dirname, 'lobby-pending-worker.mjs') });
    const issuer = await connectLobby();
    issuer.ws.send('CHALLENGE_LOBBY alice bob black byoyomi-600-10\n');
    expect(await issuer.lines.takeLine()).toBe('CHALLENGE_LOBBY:incorrect unsupported');
    issuer.ws.send('LOGIN_LOBBY alice+private-0123456789abcdef01234567+free pw\n');
    expect(await issuer.lines.takeLine()).toBe('LOGIN_LOBBY:incorrect unsupported');
  });

  test('内部予約RPCは外部ルーターから利用できない', async () => {
    for (const path of ['/private-match', '/private-match/activate', '/private-match/cancel']) {
      const response = await mf.dispatchFetch(`https://example.com${path}`, { method: 'POST', body: '{}' });
      expect(response.status).toBe(404);
    }
  });
});
