import 'dart:convert';
import 'dart:ffi';
import 'dart:typed_data';

import 'ffi/bindings.dart';
import 'observer.dart';
import 'wire.dart';

export 'wire.dart' show LiliaException;

/// How much of the device the engine sizes its buffers from.
enum ResourceProfile {
  /// A quarter of the device's memory, one compaction thread (phones, tablets, Android TV).
  mobile,

  /// A sixteenth of the device's memory, one compaction thread (watches, low-memory devices).
  constrained,

  /// The whole machine (desktops, servers).
  server,
}

typedef LiliadbProfile = ResourceProfile;

/// What survives a crash or a power loss.
enum Durability {
  /// Every acknowledged commit survives power loss.
  durable,

  /// The journal stays consistent across power loss; the latest commits may be lost.
  balanced,

  /// No sync on commit: commits survive a crash of the app, not power loss.
  performance,
}

/// How a database opens.
class LiliadbOptions {
  /// Directory of the database, created if missing.
  final String path;
  final ResourceProfile profile;
  final Durability durability;

  /// Keep a tamper-evident ledger of every record write; a database that keeps one must
  /// always be opened with it.
  final bool ledger;

  /// How long old versions stay readable, in milliseconds; a week when null, 0 keeps none.
  final int? historyRetentionMs;

  /// Commits that can be undone; none by default.
  final int undoDepth;

  /// Checkpoint when the app goes to the background ([LiliaScope]).
  final bool autoFlushOnBackground;

  const LiliadbOptions({
    required this.path,
    this.profile = ResourceProfile.mobile,
    this.durability = Durability.durable,
    this.ledger = false,
    this.historyRetentionMs,
    this.undoDepth = 0,
    this.autoFlushOnBackground = true,
  });

  Map<String, Object?> toJson() => {
        'path': path,
        'profile': profile.name,
        'durability': durability.name,
        'ledger': ledger,
        if (historyRetentionMs != null) 'history_retention_ms': historyRetentionMs,
        'undo_depth': undoDepth,
      };
}

class KvEntry {
  final String namespace;
  final Uint8List key;
  final Uint8List value;

  /// The engine sequence of the write: later writes of the database have greater ones.
  final int version;
  final int? expiresAtMs;

  const KvEntry({
    required this.namespace,
    required this.key,
    required this.value,
    required this.version,
    this.expiresAtMs,
  });

  String get valueString => utf8.decode(value);
}

class JsonEntry<T> {
  final String space;
  final String id;
  final T value;
  final int version;

  const JsonEntry({
    required this.space,
    required this.id,
    required this.value,
    required this.version,
  });
}

/// A secondary index of a JSON space.
class JsonIndexInfo {
  final String space;
  final String indexName;
  final String jsonPath;

  const JsonIndexInfo({required this.space, required this.indexName, required this.jsonPath});
}

class GraphNode<T> {
  final String graph;
  final String id;
  final String kind;
  final T metadata;

  const GraphNode({
    required this.graph,
    required this.id,
    required this.kind,
    required this.metadata,
  });
}

class FtsHit {
  final String collection;
  final String docId;
  final String snippet;
  final double rank;

  const FtsHit({
    required this.collection,
    required this.docId,
    required this.snippet,
    required this.rank,
  });
}

class SqlQueryResult<T> {
  final List<String> columns;

  /// One record per row, keyed by column.
  final List<T> rows;

  /// Always 0: SQL is read-only.
  final int rowsAffected;

  const SqlQueryResult({
    required this.columns,
    required this.rows,
    required this.rowsAffected,
  });
}

/// The sorted runs of one level of the storage.
class LevelStats {
  final int runs;
  final int bytes;

  const LevelStats({required this.runs, required this.bytes});
}

/// The shape of the storage.
class StorageStats {
  /// Level 0 first.
  final List<LevelStats> levels;

  /// Recent writes held in memory, not yet in a sorted run.
  final int memtableBytes;
  final int immutableMemtables;

  /// The newest version written.
  final int version;

  /// The oldest version still readable.
  final int oldestVersion;

  const StorageStats({
    required this.levels,
    required this.memtableBytes,
    required this.immutableMemtables,
    required this.version,
    required this.oldestVersion,
  });

  /// Bytes of the sorted runs on disk.
  int get diskBytes => levels.fold(0, (total, level) => total + level.bytes);
}

/// A LiliaDB database.
///
/// [sync] runs each call on the calling isolate and returns its result; the asynchronous
/// namespaces ([kv], [json]...) run the same calls and complete with their results. Run heavy
/// work (large scans, analytical SQL) from a background isolate.
class LiliaDatabase {
  final String path;
  final ResourceProfile profile;
  final Pointer<LiliaHandleOpaque> _handle;
  final DatabaseObserver observer = DatabaseObserver();

  bool _isClosed = false;

  /// Values read or written through [_LiliaSyncKv], for instant `peek`s from the UI.
  final Map<String, String> _memoryKvCache = {};

  static final Map<String, LiliaDatabase> _singletonRegistry = {};

  static LiliaDatabase? getDefault() => _singletonRegistry.isEmpty ? null : _singletonRegistry.values.first;

  LiliaDatabase._({
    required this.path,
    required this.profile,
    required Pointer<LiliaHandleOpaque> handle,
  }) : _handle = handle {
    _singletonRegistry[path] = this;
  }

  /// Opens (or creates) the database of directory [path]; an open database of the same path
  /// is returned as it is.
  static Future<LiliaDatabase> open({
    required String path,
    ResourceProfile profile = ResourceProfile.mobile,
    Durability durability = Durability.durable,
    bool ledger = false,
    int? historyRetentionMs,
    int undoDepth = 0,
  }) =>
      openWith(LiliadbOptions(
        path: path,
        profile: profile,
        durability: durability,
        ledger: ledger,
        historyRetentionMs: historyRetentionMs,
        undoDepth: undoDepth,
      ));

  /// Opens (or creates) the database [options] describe.
  static Future<LiliaDatabase> openWith(LiliadbOptions options) async {
    final open = _singletonRegistry[options.path];
    if (open != null && !open._isClosed) return open;
    final handle = LiliaNativeBindings.instance.open(options.toJson());
    return LiliaDatabase._(path: options.path, profile: options.profile, handle: handle);
  }

  bool get isClosed => _isClosed;

  void Function() subscribe(String pattern, void Function() callback) {
    _checkClosed();
    return observer.subscribe(pattern, callback);
  }

  void notifyChange(DatabaseChangeEvent event) {
    _checkClosed();
    observer.emit(event);
  }

  /// Writes recent commits to their sorted runs before the OS suspends the app.
  Future<void> prepareForBackground() async {
    if (_isClosed) return;
    sync.maintenance.checkpoint();
  }

  /// Closes the database once its calls return, waiting at most [timeoutMs]. Later calls
  /// throw; closing twice does nothing.
  Future<void> close({int timeoutMs = 5000}) async {
    if (_isClosed) return;
    _isClosed = true;
    _singletonRegistry.remove(path);
    observer.clear();
    _memoryKvCache.clear();
    LiliaNativeBindings.instance.close(_handle, timeoutMs);
  }

  void _checkClosed() {
    if (_isClosed) {
      throw LiliaException(code: 'CLOSED', message: 'the database at $path is closed', retryable: false);
    }
  }

  /// Runs [operation]; the value of its response of type [type].
  T _run<T>(Map<String, Object?> operation, String type) {
    _checkClosed();
    return expectValue<T>(readResponse(LiliaNativeBindings.instance.execute(_handle, operation)), type);
  }

  /// Commits the batch write [write]; its version and whether it deleted a record.
  ({int? version, bool deleted}) _write(Map<String, Object?> write) {
    final results = _run<List<dynamic>>({
      'type': 'batch',
      'operations': [write],
    }, 'mutations');
    final result = results.first as Map<String, dynamic>;
    final version = result['version'];
    return (version: version == null ? null : readVersion(version), deleted: result['deleted'] as bool);
  }

  late final sync = _LiliaSyncApi(this);

  late final kv = _LiliaAsyncKv(this);
  late final json = _LiliaAsyncJson(this);
  late final graph = _LiliaAsyncGraph(this);
  late final fts = _LiliaAsyncFts(this);
  late final sql = _LiliaAsyncSql(this);
  late final maintenance = _LiliaAsyncMaintenance(this);
}

Uint8List _bytes(Object key) => key is String ? utf8.encode(key) : key as Uint8List;

String _keyName(Object key) => key is String ? key : base64Encode(key as Uint8List);

KvEntry _kvEntry(Map<String, dynamic> entry) => KvEntry(
      namespace: entry['namespace'] as String,
      key: entry['key'] as Uint8List,
      value: entry['value'] as Uint8List,
      version: readVersion(entry['version']),
      expiresAtMs: entry['expires_at_ms'] as int?,
    );

JsonEntry<T> _jsonEntry<T>(Map<String, dynamic> entry) => JsonEntry<T>(
      space: entry['space'] as String,
      id: entry['id'] as String,
      value: entry['value'] as T,
      version: readVersion(entry['version']),
    );

const _directions = {'out': 'outbound', 'in': 'inbound', 'both': 'both'};

String _direction(String direction) =>
    _directions[direction] ??
    (throw LiliaException(code: 'INVALID_INPUT', message: 'unknown direction "$direction"', retryable: false));

class _LiliaSyncApi {
  final LiliaDatabase _db;
  _LiliaSyncApi(this._db);

  late final kv = _LiliaSyncKv(_db);
  late final json = _LiliaSyncJson(_db);
  late final graph = _LiliaSyncGraph(_db);
  late final fts = _LiliaSyncFts(_db);
  late final sql = _LiliaSyncSql(_db);
  late final maintenance = _LiliaSyncMaintenance(_db);
}

class _LiliaSyncKv {
  final LiliaDatabase _db;
  _LiliaSyncKv(this._db);

  /// The value of [key] as text: from the values this database last read or wrote, else
  /// read now.
  String? peek(String namespace, String key) {
    _db._checkClosed();
    return _db._memoryKvCache['$namespace:$key'] ?? get(namespace, key)?.valueString;
  }

  /// [key] is a [String] (UTF-8) or a [Uint8List].
  KvEntry? get(String namespace, Object key) {
    final value = _db._run<Map<String, dynamic>?>({
      'type': 'kv_get',
      'namespace': namespace,
      'key': _bytes(key),
    }, 'kv');
    if (value == null) return null;
    final entry = _kvEntry(value);
    _db._memoryKvCache['$namespace:${_keyName(key)}'] = utf8.decode(entry.value, allowMalformed: true);
    return entry;
  }

  /// Stores [value] (a [String] or a [Uint8List]) under [key]; the version of the write.
  int set(String namespace, Object key, Object value, {int? expiresAtMs}) {
    final bytes = _bytes(value);
    final written = _db._write({
      'model': 'kv_set',
      'namespace': namespace,
      'key': _bytes(key),
      'value': bytes,
      if (expiresAtMs != null) 'expires_at_ms': expiresAtMs,
    });
    final name = _keyName(key);
    _db._memoryKvCache['$namespace:$name'] = utf8.decode(bytes, allowMalformed: true);
    _db.notifyChange(DatabaseKvChangeEvent(namespace: namespace, key: name, op: 'set'));
    return written.version!;
  }

  bool delete(String namespace, Object key) {
    final written = _db._write({'model': 'kv_delete', 'namespace': namespace, 'key': _bytes(key)});
    final name = _keyName(key);
    _db._memoryKvCache.remove('$namespace:$name');
    _db.notifyChange(DatabaseKvChangeEvent(namespace: namespace, key: name, op: 'delete'));
    return written.deleted;
  }

  /// The records of [namespace] in key order, after [after], at most [limit] (every one when
  /// 0).
  List<KvEntry> scan(String namespace, {Object? after, int limit = 100}) {
    final page = _db._run<List<dynamic>>({
      'type': 'kv_scan',
      'namespace': namespace,
      if (after != null) 'after': _bytes(after),
      'limit': limit,
    }, 'kv_page');
    return page.map((entry) => _kvEntry(entry as Map<String, dynamic>)).toList();
  }
}

class _LiliaSyncJson {
  final LiliaDatabase _db;
  _LiliaSyncJson(this._db);

  JsonEntry<T>? get<T>(String space, String id) {
    final value = _db._run<Map<String, dynamic>?>({'type': 'json_get', 'space': space, 'id': id}, 'json');
    return value == null ? null : _jsonEntry<T>(value);
  }

  /// Stores the document [value] (a value `jsonEncode` accepts); the version of the write.
  int put<T>(String space, String id, T value) {
    final written = _db._write({'model': 'json_put', 'space': space, 'id': id, 'value': value});
    _db.notifyChange(DatabaseJsonChangeEvent(space: space, id: id, op: 'put'));
    return written.version!;
  }

  bool delete(String space, String id) {
    final written = _db._write({'model': 'json_delete', 'space': space, 'id': id});
    _db.notifyChange(DatabaseJsonChangeEvent(space: space, id: id, op: 'delete'));
    return written.deleted;
  }

  /// The documents of [space] in id order, after [afterId], at most [limit] (every one when 0).
  List<JsonEntry<T>> scan<T>(String space, {String? afterId, int limit = 50}) {
    final page = _db._run<List<dynamic>>({
      'type': 'json_scan',
      'space': space,
      if (afterId != null) 'after': afterId,
      'limit': limit,
    }, 'json_page');
    return page.map((entry) => _jsonEntry<T>(entry as Map<String, dynamic>)).toList();
  }

  /// The documents matching [query]: field paths (or index names) to values, all of which
  /// must match; an index on a term serves the query, else the space is scanned. After
  /// [afterId], at most [limit] (every one when 0; every document for an empty query).
  List<JsonEntry<T>> find<T>(String space, Map<String, dynamic> query, {String? afterId, int limit = 50}) {
    final page = _db._run<List<dynamic>>({
      'type': 'json_find',
      'space': space,
      'query': query,
      if (afterId != null) 'after': afterId,
      'limit': limit,
    }, 'json_page');
    return page.map((entry) => _jsonEntry<T>(entry as Map<String, dynamic>)).toList();
  }

  /// Creates index [name] of [space] over [path] (`$.field`, or an expression), backfilled
  /// from the stored documents; with [filter], a partial index of the documents it selects.
  void createIndex(String space, String name, String path, {String? filter}) {
    _db._run<Object?>({
      'type': 'json_create_index',
      'space': space,
      'name': name,
      'path': path,
      if (filter != null) 'filter': filter,
    }, 'ack');
  }

  bool dropIndex(String space, String name) =>
      _db._run<bool>({'type': 'json_drop_index', 'space': space, 'name': name}, 'bool');

  List<JsonIndexInfo> listIndexes(String space) => _db
      ._run<List<dynamic>>({'type': 'json_list_indexes', 'space': space}, 'json_indexes')
      .map((index) {
        final info = index as Map<String, dynamic>;
        return JsonIndexInfo(
          space: info['space'] as String,
          indexName: info['index_name'] as String,
          jsonPath: info['json_path'] as String,
        );
      })
      .toList();
}

class _LiliaSyncGraph {
  final LiliaDatabase _db;
  _LiliaSyncGraph(this._db);

  void setNode<T>(String graph, String id, String kind, T metadata) {
    _db._write({'model': 'graph_set_node', 'graph': graph, 'id': id, 'kind': kind, 'metadata': metadata ?? {}});
    _db.notifyChange(DatabaseGraphChangeEvent(graph: graph, op: 'node'));
  }

  GraphNode<T>? getNode<T>(String graph, String id) {
    final node = _db._run<Map<String, dynamic>?>({'type': 'graph_get_node', 'graph': graph, 'id': id}, 'graph_node');
    if (node == null) return null;
    return GraphNode<T>(
      graph: node['graph'] as String,
      id: node['id'] as String,
      kind: node['kind'] as String,
      metadata: node['metadata'] as T,
    );
  }

  bool deleteNode(String graph, String id) {
    final deleted = _db._run<bool>({'type': 'graph_delete_node', 'graph': graph, 'id': id}, 'bool');
    _db.notifyChange(DatabaseGraphChangeEvent(graph: graph, op: 'node'));
    return deleted;
  }

  void addEdge(String graph, String source, String target, String kind, {double weight = 1}) {
    _db._write({'model': 'graph_add_edge', 'graph': graph, 'source': source, 'target': target, 'kind': kind, 'weight': weight});
    _db.notifyChange(DatabaseGraphChangeEvent(graph: graph, op: 'edge'));
  }

  bool removeEdge(String graph, String source, String target, String kind) {
    final removed = _db._run<bool>(
      {'type': 'graph_remove_edge', 'graph': graph, 'source': source, 'target': target, 'kind': kind},
      'bool',
    );
    _db.notifyChange(DatabaseGraphChangeEvent(graph: graph, op: 'edge'));
    return removed;
  }

  /// The neighbours of [id]; [direction] is `out`, `in` or `both`.
  List<String> neighbors(String graph, String id, {String direction = 'out', String? kindFilter}) =>
      _db._run<List<dynamic>>({
        'type': 'graph_neighbors',
        'graph': graph,
        'node': id,
        'direction': _direction(direction),
        if (kindFilter != null) 'kind': kindFilter,
      }, 'ids').cast<String>();

  /// The nodes reachable from [startNodes] (included); [direction] is `out`, `in` or `both`.
  List<String> reachability(String graph, List<String> startNodes, {String direction = 'out'}) =>
      _db._run<List<dynamic>>({
        'type': 'graph_reachability',
        'graph': graph,
        'start_nodes': startNodes,
        'direction': _direction(direction),
      }, 'ids').cast<String>();
}

class _LiliaSyncFts {
  final LiliaDatabase _db;
  _LiliaSyncFts(this._db);

  void index(String collection, String docId, String text) {
    _db._write({'model': 'fts_index', 'collection': collection, 'doc_id': docId, 'content': text});
    _db.notifyChange(DatabaseFtsChangeEvent(collection: collection));
  }

  bool delete(String collection, String docId) {
    final deleted = _db._run<bool>({'type': 'fts_delete', 'collection': collection, 'doc_id': docId}, 'bool');
    _db.notifyChange(DatabaseFtsChangeEvent(collection: collection));
    return deleted;
  }

  List<FtsHit> search(String collection, String query, {int limit = 20}) => _db
      ._run<List<dynamic>>({'type': 'fts_search', 'collection': collection, 'query': query, 'limit': limit}, 'fts_hits')
      .map((hit) {
        final map = hit as Map<String, dynamic>;
        return FtsHit(
          collection: map['collection'] as String,
          docId: map['doc_id'] as String,
          snippet: map['snippet'] as String,
          rank: (map['rank'] as num).toDouble(),
        );
      })
      .toList();
}

class _LiliaSyncSql {
  final LiliaDatabase _db;
  _LiliaSyncSql(this._db);

  /// A read-only SQL query (DataFusion dialect); [params] bind to `$1`, `$2`...
  SqlQueryResult<T> query<T>(String sql, [List<dynamic>? params]) {
    final result = _db._run<Map<String, dynamic>>({'type': 'sql_query', 'sql': sql, 'params': params ?? []}, 'sql');
    final columns = (result['columns'] as List<dynamic>).cast<String>();
    final rows = (result['rows'] as List<dynamic>).map((row) {
      final values = row as List<dynamic>;
      return {for (var i = 0; i < columns.length; i++) columns[i]: values[i]} as T;
    }).toList();
    return SqlQueryResult<T>(columns: columns, rows: rows, rowsAffected: result['rows_affected'] as int);
  }
}

class _LiliaSyncMaintenance {
  final LiliaDatabase _db;
  _LiliaSyncMaintenance(this._db);

  /// Writes recent commits to their sorted runs.
  void checkpoint() => _db._run<Object?>({'type': 'checkpoint'}, 'ack');

  /// Verifies the ledger, fast; true without a ledger.
  bool quickCheck() => _db._run<bool>({'type': 'quick_check'}, 'bool');

  /// Reads every stored entry, verifying block checksums, and verifies the ledger.
  bool integrityCheck() => _db._run<bool>({'type': 'integrity_check'}, 'bool');

  StorageStats storageStats() {
    final stats = _db._run<Map<String, dynamic>>({'type': 'storage_stats'}, 'storage_stats');
    return StorageStats(
      levels: (stats['levels'] as List<dynamic>).map((level) {
        final map = level as Map<String, dynamic>;
        return LevelStats(runs: map['runs'] as int, bytes: map['bytes'] as int);
      }).toList(),
      memtableBytes: stats['memtable_bytes'] as int,
      immutableMemtables: stats['immutable_memtables'] as int,
      version: readVersion(stats['version']),
      oldestVersion: readVersion(stats['oldest_version']),
    );
  }
}

class _LiliaAsyncKv {
  final LiliaDatabase _db;
  _LiliaAsyncKv(this._db);

  Future<KvEntry?> get(String namespace, Object key) async => _db.sync.kv.get(namespace, key);
  Future<int> set(String namespace, Object key, Object value, {int? expiresAtMs}) async =>
      _db.sync.kv.set(namespace, key, value, expiresAtMs: expiresAtMs);
  Future<bool> delete(String namespace, Object key) async => _db.sync.kv.delete(namespace, key);
  Future<List<KvEntry>> scan(String namespace, {Object? after, int limit = 100}) async =>
      _db.sync.kv.scan(namespace, after: after, limit: limit);
}

class _LiliaAsyncJson {
  final LiliaDatabase _db;
  _LiliaAsyncJson(this._db);

  Future<JsonEntry<T>?> get<T>(String space, String id) async => _db.sync.json.get<T>(space, id);
  Future<int> put<T>(String space, String id, T value) async => _db.sync.json.put<T>(space, id, value);
  Future<bool> delete(String space, String id) async => _db.sync.json.delete(space, id);
  Future<List<JsonEntry<T>>> scan<T>(String space, {String? afterId, int limit = 50}) async =>
      _db.sync.json.scan<T>(space, afterId: afterId, limit: limit);
  Future<List<JsonEntry<T>>> find<T>(String space, Map<String, dynamic> query, {String? afterId, int limit = 50}) async =>
      _db.sync.json.find<T>(space, query, afterId: afterId, limit: limit);
  Future<void> createIndex(String space, String name, String path, {String? filter}) async =>
      _db.sync.json.createIndex(space, name, path, filter: filter);
  Future<bool> dropIndex(String space, String name) async => _db.sync.json.dropIndex(space, name);
  Future<List<JsonIndexInfo>> listIndexes(String space) async => _db.sync.json.listIndexes(space);
}

class _LiliaAsyncGraph {
  final LiliaDatabase _db;
  _LiliaAsyncGraph(this._db);

  Future<void> setNode<T>(String graph, String id, String kind, T metadata) async =>
      _db.sync.graph.setNode<T>(graph, id, kind, metadata);
  Future<GraphNode<T>?> getNode<T>(String graph, String id) async => _db.sync.graph.getNode<T>(graph, id);
  Future<bool> deleteNode(String graph, String id) async => _db.sync.graph.deleteNode(graph, id);
  Future<void> addEdge(String graph, String source, String target, String kind, {double weight = 1}) async =>
      _db.sync.graph.addEdge(graph, source, target, kind, weight: weight);
  Future<bool> removeEdge(String graph, String source, String target, String kind) async =>
      _db.sync.graph.removeEdge(graph, source, target, kind);
  Future<List<String>> neighbors(String graph, String id, {String direction = 'out', String? kindFilter}) async =>
      _db.sync.graph.neighbors(graph, id, direction: direction, kindFilter: kindFilter);
  Future<List<String>> reachability(String graph, List<String> startNodes, {String direction = 'out'}) async =>
      _db.sync.graph.reachability(graph, startNodes, direction: direction);
}

class _LiliaAsyncFts {
  final LiliaDatabase _db;
  _LiliaAsyncFts(this._db);

  Future<void> index(String collection, String docId, String text) async => _db.sync.fts.index(collection, docId, text);
  Future<bool> delete(String collection, String docId) async => _db.sync.fts.delete(collection, docId);
  Future<List<FtsHit>> search(String collection, String query, {int limit = 20}) async =>
      _db.sync.fts.search(collection, query, limit: limit);
}

class _LiliaAsyncSql {
  final LiliaDatabase _db;
  _LiliaAsyncSql(this._db);

  Future<SqlQueryResult<T>> query<T>(String sql, [List<dynamic>? params]) async => _db.sync.sql.query<T>(sql, params);
}

class _LiliaAsyncMaintenance {
  final LiliaDatabase _db;
  _LiliaAsyncMaintenance(this._db);

  Future<void> checkpoint() async => _db.sync.maintenance.checkpoint();
  Future<bool> quickCheck() async => _db.sync.maintenance.quickCheck();
  Future<bool> integrityCheck() async => _db.sync.maintenance.integrityCheck();
  Future<StorageStats> storageStats() async => _db.sync.maintenance.storageStats();
}
