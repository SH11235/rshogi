import worker, { Lobby as RustLobby, GameRoom, RateLimiter } from '../../build/worker/shim.mjs';
export { GameRoom, RateLimiter };
export default worker;

// 本番の Rust ハンドラへ、復帰と古い attachment・遅延 close を注入する。
export class Lobby {
  constructor(state, env) {
    this.state = state;
    this.env = { ...env, CLOCK_PRESETS: JSON.stringify([
      { game_name: 'test', kind: 'countdown', total_time_sec: 600, byoyomi_sec: 10 },
      { game_name: 'byoyomi-600-10', kind: 'countdown', total_time_sec: 600, byoyomi_sec: 10 },
    ]) };
    this.holdClose = false;
    this.closes = [];
    const wrap = (object, overrides) => new Proxy(object, {
      get(target, key) {
        if (Object.hasOwn(overrides, key)) return overrides[key];
        const value = Reflect.get(target, key, target);
        return typeof value === 'function' && key !== 'constructor' ? value.bind(target) : value;
      },
    });
    this.sockets = new WeakMap();
    this.socket = ws => {
      if (!this.sockets.has(ws)) this.sockets.set(ws, wrap(ws, {
        close: (code, reason) => {
          this.closes.push({ code, reason });
          if (!this.holdClose) return ws.close(code, reason);
        },
      }));
      return this.sockets.get(ws);
    };
    this.context = wrap(state, {
      getWebSockets: (...args) => state.getWebSockets(...args).map(this.socket),
    });
    this.inner = new RustLobby(this.context, this.env);
  }
  async fetch(request) {
    if (new URL(request.url).pathname !== '/__test') return this.inner.fetch(request);
    const command = await request.json();
    if (command.holdClose !== undefined) this.holdClose = command.holdClose;
    for (const ws of this.state.getWebSockets()) {
      const att = ws.deserializeAttachment();
      if (!att?.Pending) continue;
      if (command.legacy) delete att.Pending.expires_at_ms;
      if (command.expire) att.Pending.expires_at_ms = Date.now() - 1;
      if (command.deadline) att.Pending.expires_at_ms = Date.now() + command.deadline;
      ws.serializeAttachment(att);
    }
    if (command.reset) this.inner = new RustLobby(this.context, this.env);
    if (command.alarm) {
      await this.state.storage.deleteAlarm();
      await this.inner.alarm();
    }
    return Response.json({
      alarm: await this.state.storage.getAlarm(), closes: this.closes,
      attachments: this.state.getWebSockets().map(ws => ws.deserializeAttachment()),
    });
  }
  alarm() { return this.inner.alarm(); }
  webSocketMessage(ws, message) { return this.inner.webSocketMessage(this.socket(ws), message); }
  webSocketClose(ws, code, reason, clean) { return this.inner.webSocketClose(ws, code, reason, clean); }
  webSocketError(ws, error) { return this.inner.webSocketError(ws, error); }
}
