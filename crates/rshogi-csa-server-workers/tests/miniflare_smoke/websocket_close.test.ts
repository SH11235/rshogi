import { afterEach, expect, it } from 'vitest';
import { Miniflare } from 'miniflare';
import NodeWebSocket from 'ws';

// 終局処理を含まない最小ケース。最初の client frame より前でも close を受け取れるか。
let mf: Miniflare | undefined;
let client: NodeWebSocket | undefined;
afterEach(async () => {
  client?.terminate();
  await mf?.dispose();
});

it.each([1000, 1011])('未送信の hibernatable WS の close フレームを受け取る (%s)', async code => {
  mf = new Miniflare({
    modules: true,
    compatibilityDate: '2026-04-21',
    durableObjects: { ROOM: { className: 'CloseRoom', useSQLite: true } },
    script: `
      export class CloseRoom {
        constructor(state) { this.state = state; }
        fetch(request) {
          const url = new URL(request.url);
          if (url.pathname === '/close') {
            for (const ws of this.state.getWebSockets()) ws.close(Number(url.searchParams.get('code')), 'test close');
            return new Response('closed');
          }
          const pair = new WebSocketPair();
          this.state.acceptWebSocket(pair[1]);
          return new Response(null, { status: 101, webSocket: pair[0] });
        }
        webSocketMessage() {}
        webSocketClose(ws, code, reason) { ws.close(code, reason); }
      }
      export default {
        fetch(request, env) { return env.ROOM.get(env.ROOM.idFromName('close')).fetch(request); }
      };
    `,
  });
  const url = new URL(await mf.ready);
  url.protocol = 'ws:';
  // close frame が到着した後の TCP FIN 待ちだけを短縮する。
  const options = { handshakeTimeout: 5000, closeTimeout: 1000 };
  client = new NodeWebSocket(url, options);
  const closed = new Promise<number>((resolve, reject) => {
    const timer = setTimeout(() => {
      const observed = client as unknown as { _closeFrameReceived: boolean; _closeCode: number };
      reject(new Error(`close timeout: state=${client?.readyState}, received=${observed._closeFrameReceived}, code=${observed._closeCode}`));
    }, 5000);
    client!.once('close', code => { clearTimeout(timer); resolve(code); });
  });
  await new Promise<void>((resolve, reject) => {
    client!.once('open', resolve);
    client!.once('error', reject);
  });
  await mf.dispatchFetch(`https://test/close?code=${code}`);
  expect(await closed).toBe(code);
});
