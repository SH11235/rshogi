import worker, { GameRoom as RustGameRoom, Lobby, RateLimiter } from '../../build/worker/shim.mjs';
export { Lobby, RateLimiter };
export default worker;

// 実際の DO ハンドラに対し、保存境界と送信境界で障害を注入する。
export class GameRoom {
  constructor(state, env) {
    this.state = state;
    this.faults = {};
    this.sockets = new WeakMap();
    const wrap = (object, overrides) => new Proxy(object, {
      get(target, key) {
        if (Object.hasOwn(overrides, key)) return overrides[key];
        const value = Reflect.get(target, key, target);
        // binding の型判定は constructor.name を読むため、constructor は bind しない。
        return typeof value === 'function' && key !== 'constructor' ? value.bind(target) : value;
      },
    });
    const socket = (ws) => {
      if (!this.sockets.has(ws)) this.sockets.set(ws, wrap(ws, {
        send: (line) => {
          const att = ws.deserializeAttachment();
          if (this.faults.sendRole === att?.role && String(line).trim() === this.faults.sendLine) {
            throw new Error('injected WS send failure');
          }
          return ws.send(line);
        },
        serializeAttachment: (att) => {
          if (this.faults.persistRole === att?.role && att?.terminal_sent?.includes(this.faults.persistLine)) {
            this.faults.persistRole = undefined;
            throw new Error('injected attachment persistence failure');
          }
          return ws.serializeAttachment(att);
        },
      }));
      return this.sockets.get(ws);
    };
    const sql = wrap(state.storage.sql, {
      exec: (query, ...args) => {
        if (this.faults.beforeMove && query.startsWith('INSERT INTO moves')) {
          this.faults.beforeMove = false;
          throw new Error('injected failure before move persistence');
        }
        const result = state.storage.sql.exec(query, ...args);
        if (this.faults.afterMove && query.startsWith('INSERT INTO moves')) {
          this.faults.afterMove = false;
          throw new Error('injected interruption after move persistence');
        }
        return result;
      },
    });
    const storage = wrap(state.storage, { sql });
    this.context = wrap(state, {
      storage,
      getWebSockets: (...args) => state.getWebSockets(...args).map(socket),
    });
    this.env = { ...env, KIFU_BUCKET: wrap(env.KIFU_BUCKET, {
      put: (...args) => {
        if (this.faults.r2) throw new Error('injected R2 failure');
        return env.KIFU_BUCKET.put(...args);
      },
    }) };
    this.inner = new RustGameRoom(this.context, this.env);
  }
  async fetch(request) {
    if (new URL(request.url).pathname === '/__test') {
      const command = await request.json();
      if (command.faults) this.faults = command.faults;
      if (command.grace) {
        await this.state.storage.put('pending_alarm_kind', 'GraceExpired');
        await this.state.storage.put('grace_registry', {
          disconnected_handle: 'white', disconnected_color: 'white', expected_token: 'token',
          deadline_ms: Date.now(), game_summary_for_disconnected: '',
          snapshot: { position_section: '', black_remaining_ms: 600000, white_remaining_ms: 600000,
            current_turn: 'black', last_move: null },
        });
      }
      // 接続と永続状態を保持して Rust インスタンスを再構築し、メモリ上のコアを破棄する。
      if (command.reset) this.inner = new RustGameRoom(this.context, this.env);
      if (command.alarm) await this.inner.alarm();
      return Response.json({
        finished: await this.state.storage.get('finished') ?? null,
        kind: await this.state.storage.get('pending_alarm_kind') ?? null,
        pending: await this.state.storage.get('export_pending') ?? null,
        alarm: await this.state.storage.getAlarm(),
        moves: this.state.storage.sql.exec('SELECT COUNT(*) AS n FROM moves').one().n,
        beforeMoveArmed: Boolean(this.faults.beforeMove),
      });
    }
    return this.inner.fetch(request);
  }
  alarm() { return this.inner.alarm(); }
  async webSocketMessage(ws, message) {
    try {
      await this.inner.webSocketMessage(ws, message);
    } catch (error) {
      // 注入した中断では接続を保持し、次のイベントで再入させる。
      if (!String(error).includes('injected')) throw error;
    }
  }
  webSocketClose(ws, code, reason, clean) { return this.inner.webSocketClose(ws, code, reason, clean); }
  webSocketError(ws, error) { return this.inner.webSocketError(ws, error); }
}
