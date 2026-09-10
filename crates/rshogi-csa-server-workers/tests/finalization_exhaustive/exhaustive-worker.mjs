import worker, { GameRoom as RustGameRoom, Lobby, RateLimiter } from '../../build/worker/shim.mjs';
export { Lobby, RateLimiter };
export default worker;

const STORAGE_EFFECTS = ['get', 'put', 'delete', 'getAlarm', 'setAlarm', 'deleteAlarm'];
const SOCKET_EFFECTS = ['send', 'serializeAttachment', 'close'];
const MAX_ALARM_RETRIES = 6;

// 計画を仕込んだ後の副作用操作に通し番号を振り、N 番目で障害を注入する。
// fail: N 番目だけ失敗させる。
// crash: N 番目以降をすべて失敗させ、ハンドラ終了後に Rust インスタンスを作り直して
//        メモリ上の状態を捨てる (isolate 破棄を模す)。
export class GameRoom {
  constructor(state, env) {
    this.state = state;
    this.plan = null;
    this.ops = [];
    this.injectedAt = null;
    this.dead = false;
    this.active = 0;
    this.alarmFailures = 0;
    this.sockets = new WeakMap();

    const fault = (name) => {
      if (!this.plan) return null;
      this.ops.push(name);
      if (this.dead) return new Error(`injected crash (dead) ${name}`);
      if (this.injectedAt === null && this.ops.length === this.plan.at) {
        this.injectedAt = { index: this.ops.length, name };
        if (this.plan.mode === 'crash') this.dead = true;
        return new Error(`injected ${this.plan.mode} at ${this.ops.length}:${name}`);
      }
      return null;
    };
    const wrap = (object, prefix, names, overrides = {}) => new Proxy(object, {
      get(target, key) {
        if (Object.hasOwn(overrides, key)) return overrides[key];
        const value = Reflect.get(target, key, target);
        if (typeof value !== 'function' || key === 'constructor') return value;
        const bound = value.bind(target);
        if (names !== 'all' && !names.includes(key)) return bound;
        return (...args) => {
          const error = fault(`${prefix}.${String(key)}`);
          if (error) {
            if (prefix.startsWith('r2')) return Promise.reject(error);
            throw error;
          }
          return bound(...args);
        };
      },
    });
    const socket = (ws) => {
      if (!this.sockets.has(ws)) this.sockets.set(ws, wrap(ws, 'ws', SOCKET_EFFECTS));
      return this.sockets.get(ws);
    };
    const sql = wrap(state.storage.sql, 'sql', ['exec']);
    const storage = wrap(state.storage, 'storage', STORAGE_EFFECTS, { sql });
    this.context = wrap(state, 'state', [], {
      storage,
      getWebSockets: (...args) => state.getWebSockets(...args).map(socket),
    });
    this.env = {
      ...env,
      KIFU_BUCKET: wrap(env.KIFU_BUCKET, 'r2.kifu', 'all'),
      FLOODGATE_HISTORY_BUCKET: wrap(env.FLOODGATE_HISTORY_BUCKET, 'r2.history', 'all'),
    };
    this.inner = new RustGameRoom(this.context, this.env);
  }

  async run(handler) {
    this.active += 1;
    try {
      return await handler();
    } catch (error) {
      if (!String(error).includes('injected')) throw error;
      return error;
    } finally {
      this.active -= 1;
      if (this.dead && this.active === 0) {
        this.dead = false;
        this.inner = new RustGameRoom(this.context, this.env);
      }
    }
  }

  // Cloudflare と同じく予約を消費してから呼び、失敗時は予約を戻して再試行扱いにする。
  async fireAlarm() {
    const at = await this.state.storage.getAlarm();
    if (at === null) return { fired: false };
    await this.state.storage.deleteAlarm();
    const outcome = await this.run(() => this.inner.alarm());
    const failed = outcome instanceof Error;
    if (failed) {
      this.alarmFailures += 1;
      if (this.alarmFailures <= MAX_ALARM_RETRIES && (await this.state.storage.getAlarm()) === null) {
        await this.state.storage.setAlarm(Date.now() + 60_000);
      }
    } else {
      this.alarmFailures = 0;
    }
    return { fired: true, failed };
  }

  async fetch(request) {
    if (new URL(request.url).pathname !== '/__test') {
      return this.run(() => this.inner.fetch(request));
    }
    const command = await request.json();
    if (command.plan) {
      this.plan = command.plan;
      this.ops = [];
      this.injectedAt = null;
    }
    const fired = command.fireAlarm ? await this.fireAlarm() : null;
    return Response.json({
      fired,
      active: this.active,
      ops: this.ops,
      injectedAt: this.injectedAt,
      finished: await this.state.storage.get('finished') ?? null,
      finalizing: await this.state.storage.get('finalizing') ?? null,
      kind: await this.state.storage.get('pending_alarm_kind') ?? null,
      alarm: await this.state.storage.getAlarm(),
      moves: this.state.storage.sql.exec('SELECT COUNT(*) AS n FROM moves').one().n,
    });
  }

  alarm() { return this.run(() => this.inner.alarm()); }
  webSocketMessage(ws, message) { return this.run(() => this.inner.webSocketMessage(ws, message)); }
  webSocketClose(ws, code, reason, clean) { return this.run(() => this.inner.webSocketClose(ws, code, reason, clean)); }
  webSocketError(ws, error) { return this.run(() => this.inner.webSocketError(ws, error)); }
}
