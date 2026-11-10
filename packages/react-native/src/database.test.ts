import test from 'node:test';
import assert from 'node:assert/strict';
import { NativeModules, AppState } from 'react-native';

// Mock React Native modules before importing LiliaDatabase
const mockStore = new Map<string, any>();
const mockKv = new Map<string, any>();
const mockJson = new Map<string, any>();
const mockGraphNodes = new Map<string, any>();
const mockGraphEdges = new Map<string, any>();
let appStateCallback: ((state: string) => void) | null = null;
let backgroundCallCount = 0;

(NativeModules as any).LiliaReactNativeModule = {
  async openDatabase(opts: any) {
    const handle = 'mock-handle-123';
    mockStore.set(handle, opts);
    return { handle, path: opts.path };
  },
  async close(handle: string, timeoutMs: number) {
    mockStore.delete(handle);
    return true;
  },
  async prepareForBackground(handle: string) {
    backgroundCallCount++;
    return true;
  },
  async kvSet(handle: string, namespace: string, keyHex: string, valHex: string, expiresAtMs?: number | null) {
    const k = `${namespace}:${keyHex}`;
    const version = (mockKv.get(k)?.version ?? 0) + 1;
    mockKv.set(k, {
      namespace,
      keyHex,
      valueHex: valHex,
      version,
      expiresAtMs: expiresAtMs ?? null,
    });
    return version;
  },
  async kvGet(handle: string, namespace: string, keyHex: string) {
    const k = `${namespace}:${keyHex}`;
    return mockKv.get(k) ?? null;
  },
  async jsonPut(handle: string, space: string, id: string, valueJson: string) {
    const k = `${space}:${id}`;
    const version = (mockJson.get(k)?.version ?? 0) + 1;
    mockJson.set(k, {
      space,
      id,
      valueJson,
      version,
    });
    return version;
  },
  async jsonGet(handle: string, space: string, id: string) {
    const k = `${space}:${id}`;
    return mockJson.get(k) ?? null;
  },
  async graphSetNode(handle: string, graph: string, id: string, kind: string, metaJson: string) {
    mockGraphNodes.set(`${graph}:${id}`, { graph, id, kind, metadataJson: metaJson });
  },
  async graphGetNode(handle: string, graph: string, id: string) {
    return mockGraphNodes.get(`${graph}:${id}`) ?? null;
  },
  async graphAddEdge(handle: string, graph: string, source: string, target: string, kind: string) {
    mockGraphEdges.set(`${graph}:${source}:${target}:${kind}`, { graph, source, target, kind });
  },
  async graphNeighbors(handle: string, graph: string, node: string, dir: number, kind?: string) {
    return ['user:target-1', 'user:target-2'];
  },
  async ftsSearch(handle: string, col: string, query: string, limit: number) {
    return [
      { collection: col, docId: 'doc-1', snippet: '<b>test</b> snippet', rank: -1.25 }
    ];
  },
  async sqlQuery(handle: string, sql: string, paramsJson?: string | null) {
    return {
      columns: ['id', 'name'],
      rowsJson: [JSON.stringify([1, 'Alice']), JSON.stringify([2, 'Bob'])],
      rowsAffected: 0,
    };
  },
  async getFileSize(handle: string) {
    return {
      mainBytes: 10240,
      walBytes: 4096,
      shmBytes: 32768,
      totalBytes: 47104,
    };
  },
  async vacuum(handle: string) {
    return true;
  },
};

(AppState as any).addEventListener = (event: string, callback: (state: string) => void) => {
  if (event === 'change') {
    appStateCallback = callback;
  }
  return {
    remove: () => {
      appStateCallback = null;
    },
  };
};

// Mock C++ JSI runtime hook `__lilia_open_sync`
(global as any).__lilia_open_sync = (opts: any) => {
  const syncStore = new Map<string, string>();
  const syncJsonStore = new Map<string, string>();

  return {
    path: opts.path,
    isClosed: false,
    kvGet(ns: string, key: string) {
      return syncStore.get(`${ns}:${key}`) ?? null;
    },
    kvSet(ns: string, key: string, val: string, exp: number) {
      syncStore.set(`${ns}:${key}`, val);
      return 1;
    },
    kvDelete(ns: string, key: string) {
      return syncStore.delete(`${ns}:${key}`);
    },
    jsonGet(space: string, id: string) {
      return syncJsonStore.get(`${space}:${id}`) ?? null;
    },
    jsonPut(space: string, id: string, docJson: string) {
      syncJsonStore.set(`${space}:${id}`, docJson);
      return 1;
    },
    jsonDelete(space: string, id: string) {
      return syncJsonStore.delete(`${space}:${id}`);
    },
    jsonFind(space: string, queryJson: string) {
      const results: any[] = [];
      for (const [k, v] of syncJsonStore.entries()) {
        if (k.startsWith(`${space}:`)) {
          results.push({ space, id: k.split(':')[1], value: JSON.parse(v), version: 1 });
        }
      }
      return JSON.stringify(results);
    },
    graphSetNode(graph: string, id: string, kind: string, meta: string) {},
    graphGetNode(graph: string, id: string) {
      return JSON.stringify({ graph, id, kind: 'person', metadata: { age: 30 } });
    },
    graphAddEdge(g: string, s: string, t: string, k: string) {},
    graphNeighbors(g: string, n: string, d: number) {
      return JSON.stringify(['node:b', 'node:c']);
    },
    ftsSearch(col: string, q: string, lim: number) {
      return JSON.stringify([{ collection: col, docId: 'doc-jsi-1', snippet: 'match', rank: -0.5 }]);
    },
    sqlQuery(sql: string) {
      return {
        columnsJson: JSON.stringify(['colA', 'colB']),
        rowsJson: JSON.stringify([['val1', 'val2']]),
        rowsAffected: 0,
      };
    },
    vacuum() {},
    checkpoint() {},
    getFileSize() {
      return { mainBytes: 20480, walBytes: 4096, shmBytes: 32768, totalBytes: 57344 };
    },
    close() {},
  };
};

// Now import the module under test
import { LiliaDatabase } from './database.js';
import { resolveDatabasePath } from './sandbox.js';

test('Sandbox path resolution respects OS conventions and App Groups', () => {
  // Explicit path
  assert.equal(resolveDatabasePath({ path: '/tmp/custom.lilia' }), '/tmp/custom.lilia');

  // iOS sandbox resolution
  const iosPath = resolveDatabasePath({ name: 'myapp', ios: { directory: 'applicationSupport' } });
  assert.ok(iosPath.includes('myapp.lilia'));

  // iOS App Groups container
  const groupPath = resolveDatabasePath({
    name: 'widget_data',
    ios: { groupContainerIdentifier: 'group.com.liliadb.demo' },
  });
  assert.ok(groupPath.startsWith('group://group.com.liliadb.demo/'));

  // Android noBackup default
  const androidPath = resolveDatabasePath({ name: 'android_db', android: { directory: 'noBackup' } });
  assert.ok(androidPath.includes('android_db.lilia'));
});

test('Synchronous C++ JSI Multimodel Engine (KV, JSON, Graph, FTS, SQL, Maintenance)', async () => {
  const db = await LiliaDatabase.open({ path: 'test-jsi.lilia' });

  // 1. Sync KV
  const v1 = db.sync.kv.set('auth', 'jwt', 'token-abc');
  assert.equal(v1, 1);
  assert.equal(db.sync.kv.get('auth', 'jwt'), 'token-abc');
  assert.equal(db.sync.kv.peek('auth', 'jwt'), 'token-abc');
  const deleted = db.sync.kv.delete('auth', 'jwt');
  assert.equal(deleted, true);
  assert.equal(db.sync.kv.get('auth', 'jwt'), null);

  // 2. Sync JSON
  const jv = db.sync.json.put('users', 'u1', { name: 'Grace Hopper', rank: 'Admiral' });
  assert.equal(jv, 1);
  const doc = db.sync.json.get<{ name: string; rank: string }>('users', 'u1');
  assert.ok(doc !== null);
  assert.equal(doc.name, 'Grace Hopper');

  // 3. Sync Graph
  db.sync.graph.setNode('social', 'alice', 'user', { bio: 'Engineer' });
  const node = db.sync.graph.getNode<{ age: number }>('social', 'alice');
  assert.ok(node !== null);
  assert.equal(node.id, 'alice');
  const neighbors = db.sync.graph.neighbors('social', 'alice', 'out');
  assert.deepEqual(neighbors, ['node:b', 'node:c']);

  // 4. Sync FTS5
  const hits = db.sync.fts.search('docs', 'quantum');
  assert.equal(hits.length, 1);
  assert.equal(hits[0].docId, 'doc-jsi-1');

  // 5. Sync SQL
  const sqlRes = db.sync.sql.query<{ colA: string; colB: string }>('SELECT colA, colB FROM data');
  assert.deepEqual(sqlRes.columns, ['colA', 'colB']);
  assert.equal(sqlRes.rows.length, 1);
  assert.equal(sqlRes.rows[0].colA, 'val1');

  // 6. Sync Maintenance
  const metrics = db.sync.maintenance.getFileSize();
  assert.equal(metrics.mainBytes, 20480);
  assert.equal(metrics.totalBytes, 57344);
  db.sync.maintenance.vacuum();
  db.sync.maintenance.checkpoint();

  await db.close();
});

test('Asynchronous Multimodel API (KV, JSON, Graph, FTS, SQL, Maintenance)', async () => {
  const db = await LiliaDatabase.open({ path: 'test-async.lilia' });

  // 1. Async KV
  await db.kv.set('settings', 'theme', 'dark');
  const entry = await db.kv.get('settings', 'theme');
  assert.ok(entry !== null);
  assert.equal(new TextDecoder().decode(entry.value), 'dark');

  // 2. Async JSON
  await db.json.put('projects', 'p1', { title: 'LiliaDB RN' });
  const p1 = await db.json.get<{ title: string }>('projects', 'p1');
  assert.ok(p1 !== null);
  assert.equal(p1.value.title, 'LiliaDB RN');

  // 3. Async Graph
  await db.graph.setNode('org', 'ceo', 'executive');
  const ceo = await db.graph.getNode('org', 'ceo');
  assert.ok(ceo !== null);
  const nList = await db.graph.neighbors('org', 'ceo');
  assert.deepEqual(nList, ['user:target-1', 'user:target-2']);

  // 4. Async FTS5
  const searchHits = await db.fts.search('articles', 'rust');
  assert.equal(searchHits.length, 1);
  assert.equal(searchHits[0].docId, 'doc-1');

  // 5. Async SQL
  const sqlRes = await db.sql.query<{ id: number; name: string }>('SELECT id, name FROM users');
  assert.equal(sqlRes.rows.length, 2);

  // 6. Async Maintenance
  const metrics = await db.maintenance.getFileSize();
  assert.equal(metrics.mainBytes, 10240);
  assert.equal(metrics.totalBytes, 47104);

  await db.close();
});

test('Fast Refresh singleton registry prevents SQLITE_BUSY', async () => {
  (global as any).__DEV__ = true;

  const db1 = await LiliaDatabase.open({
    path: 'shared-singleton.lilia',
    dev: { enableFastRefreshSingleton: true },
  });

  // Second open on identical path returns cached instance without re-opening
  const db2 = await LiliaDatabase.open({
    path: 'shared-singleton.lilia',
    dev: { enableFastRefreshSingleton: true },
  });

  assert.equal(db1, db2);

  await db1.close();
});
