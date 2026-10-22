import worker, { GameRoom, Lobby, RateLimiter } from '../../build/worker/shim.mjs';
export { GameRoom, Lobby, RateLimiter };

let faults = {};
const wrap = (object, overrides) => new Proxy(object, {
  get(target, key) {
    if (Object.hasOwn(overrides, key)) return overrides[key];
    const value = Reflect.get(target, key, target);
    // Rust の binding 型判定が constructor.name を読む。
    return typeof value === 'function' && key !== 'constructor' ? value.bind(target) : value;
  },
});

export default class extends worker {
  constructor(ctx, env) {
    const primary = env.KIFU_BUCKET;
    const backup = env.KIFU_BACKUP_BUCKET;
    super(ctx, { ...env,
      GAMES_SEARCH_DB: wrap(env.GAMES_SEARCH_DB, {
        prepare: (sql) => {
          const statement = env.GAMES_SEARCH_DB.prepare(sql);
          if (faults.d1Read && sql.startsWith('SELECT ') && sql.includes('games_search_index WHERE game_id')) {
            const failRead = (query) => wrap(query, {
              bind: (...args) => failRead(query.bind(...args)),
              first: async () => { throw new Error('injected D1 read failure'); },
            });
            return failRead(statement);
          }
          return statement;
        },
      }),
      KIFU_BUCKET: wrap(primary, {
        list: async (options) => {
          if (options?.prefix === 'games-index/' && faults.restoreIndexDelayMs) {
            await new Promise(resolve => setTimeout(resolve, faults.restoreIndexDelayMs));
          }
          return primary.list(options);
        },
        put: async (key, ...args) => {
          if (faults.primaryPut === key) {
            delete faults.primaryPut;
            throw new Error('injected primary PUT failure');
          }
          return primary.put(key, ...args);
        },
      }),
      ...(backup ? { KIFU_BACKUP_BUCKET: wrap(backup, {
        put: async (key, ...args) => {
          if (key === 'control/kifu-backup-v1.json' && faults.finalCheckpointPut) {
            const state = await new Response(args[0]).json();
            if (state.lease_until_ms === 0) {
              delete faults.finalCheckpointPut;
              throw new Error('injected final checkpoint PUT failure');
            }
          }
          if (key.startsWith('snapshots/')) {
            if (faults.snapshotDelayMs) {
              await new Promise(resolve => setTimeout(resolve, faults.snapshotDelayMs));
            }
            if (faults.backupPut) {
              delete faults.backupPut;
              throw new Error('injected backup PUT failure');
            }
            if (await backup.head(key)) throw new Error('injected snapshot overwrite lock');
          }
          return backup.put(key, ...args);
        },
      }) } : {}),
    });
  }

  async fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === '/__test/faults') {
      faults = await request.json();
      return new Response('ok');
    }
    if (path === '/__test/cron') {
      const scheduledTime = Math.floor(Date.now() / 3_600_000) * 3_600_000 + 15 * 60_000;
      await this.scheduled({ cron: '0,15,30,45 * * * *', scheduledTime, noRetry() {} });
      return new Response('ok');
    }
    return super.fetch(request);
  }
}
