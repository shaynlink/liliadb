import 'dart:convert';
import 'dart:ffi';
import 'dart:typed_data';
import 'ffi/bindings.dart';
import 'observer.dart';
import 'secure_storage.dart';

enum ResourceProfile {
  /// 8MB SQLite cache, optimized WAL (Smartphones, Tablets, Android TV).
  mobile,

  /// 2MB SQLite cache, mmap disabled (watchOS, Wear OS, Apple TV).
  constrained,

  /// 64MB SQLite cache, 256MB mmap (Desktop, Server).
  server,
}

typedef LiliadbProfile = ResourceProfile;

class LiliadbOptions {
  final String path;
  final ResourceProfile profile;
  final bool autoFlushOnBackground;
  final bool useSecureStorage;

  const LiliadbOptions({
    required this.path,
    this.profile = ResourceProfile.mobile,
    this.autoFlushOnBackground = true,
    this.useSecureStorage = false,
  });
}

class KvEntry {
  final String namespace;
  final Uint8List key;
  final Uint8List value;
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
  final List<T> rows;
  final int rowsAffected;

  const SqlQueryResult({
    required this.columns,
    required this.rows,
    required this.rowsAffected,
  });
}

class StorageMetrics {
  final int mainBytes;
  final int walBytes;
  final int shmBytes;
  final int totalBytes;

  const StorageMetrics({
    required this.mainBytes,
    required this.walBytes,
    required this.shmBytes,
    required this.totalBytes,
  });
}

/// The core LiliaDB Database instance for Flutter.
///
/// Exposes both:
/// - [sync]: Sub-millisecond synchronous execution via direct Dart FFI (Zero FOUC)
/// - Asynchronous APIs: Non-blocking executions for heavier tasks
class LiliaDatabase {
  final String path;
  final ResourceProfile profile;
  final Pointer<LiliaHandleOpaque> _handle;
  final DatabaseObserver observer = DatabaseObserver();

  bool _isClosed = false;

  // In-memory cache for synchronous peek and instant UI rendering
  final Map<String, String> _memoryKvCache = {};
  final Map<String, Map<String, String>> _memoryJsonCache = {};

  // Global Singleton Registry
  static final Map<String, LiliaDatabase> _singletonRegistry = {};

  static LiliaDatabase? getDefault() {
    if (_singletonRegistry.isNotEmpty) {
      return _singletonRegistry.values.first;
    }
    return null;
  }

  LiliaDatabase._({
    required this.path,
    required this.profile,
    required Pointer<LiliaHandleOpaque> handle,
  }) : _handle = handle {
    _singletonRegistry[path] = this;
  }

  /// Opens a LiliaDB database file with the given hardware profile and optional encryption.
  static Future<LiliaDatabase> open({
    required String path,
    ResourceProfile profile = ResourceProfile.mobile,
    bool useSecureStorage = false,
    Uint8List? encryptionKey,
    String? passphrase,
  }) async {
    // Check singleton registry
    if (_singletonRegistry.containsKey(path) && !_singletonRegistry[path]!._isClosed) {
      return _singletonRegistry[path]!;
    }

    Uint8List? resolvedKey = encryptionKey;
    if (useSecureStorage && resolvedKey == null && passphrase == null) {
      final dbName = path.split(RegExp(r'[/\\]')).last;
      resolvedKey = await LiliaSecureStorage.getOrCreateKey(dbName);
    }

    final bindings = LiliaNativeBindings.instance;
    final profileCode = switch (profile) {
      ResourceProfile.constrained => 1,
      ResourceProfile.server => 2,
      ResourceProfile.mobile => 0,
    };

    final handle = bindings.open(
      path: path,
      profileCode: profileCode,
      keyBytes: resolvedKey,
      passphrase: passphrase,
    );

    return LiliaDatabase._(
      path: path,
      profile: profile,
      handle: handle,
    );
  }

  bool get isClosed => _isClosed;

  // ==========================================
  // REACTIVE OBSERVER SUBSCRIPTION
  // ==========================================

  void Function() subscribe(String pattern, void Function() callback) {
    _checkClosed();
    return observer.subscribe(pattern, callback);
  }

  void notifyChange(DatabaseChangeEvent event) {
    _checkClosed();
    observer.emit(event);
  }

  // ==========================================
  // MOBILE LIFECYCLE HOOKS
  // ==========================================

  /// Checkpoints WAL and flushes memory before iOS/Android app suspension.
  Future<void> prepareForBackground() async {
    if (_isClosed) return;
    LiliaNativeBindings.instance.prepareForBackground(_handle);
  }

  Future<void> close({int timeoutMs = 5000}) async {
    if (_isClosed) return;
    _isClosed = true;
    _singletonRegistry.remove(path);
    observer.clear();
    _memoryKvCache.clear();
    _memoryJsonCache.clear();
    LiliaNativeBindings.instance.close(_handle, timeoutMs: timeoutMs);
  }

  void _checkClosed() {
    if (_isClosed) {
      throw StateError('Database at $path is closed.');
    }
  }

  // ==========================================
  // SYNCHRONOUS C ABI / FFI API (0 ms, SUB-MILLISECOND)
  // ==========================================

  late final sync = _LiliaSyncApi(this);

  // ==========================================
  // ASYNCHRONOUS API (WORKER / BACKGROUND POOL)
  // ==========================================

  late final kv = _LiliaAsyncKv(this);
  late final json = _LiliaAsyncJson(this);
  late final graph = _LiliaAsyncGraph(this);
  late final fts = _LiliaAsyncFts(this);
  late final sql = _LiliaAsyncSql(this);
  late final maintenance = _LiliaAsyncMaintenance(this);
}

// --------------------------------------------------
// SYNCHRONOUS SUB-ENGINE
// --------------------------------------------------

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

  String? peek(String namespace, String key) {
    _db._checkClosed();
    final cacheKey = '$namespace:$key';
    if (_db._memoryKvCache.containsKey(cacheKey)) {
      return _db._memoryKvCache[cacheKey];
    }
    final raw = get(namespace, key);
    return raw?.valueString;
  }

  KvEntry? get(String namespace, dynamic key) {
    _db._checkClosed();
    final keyBytes = key is String ? utf8.encode(key) : key as Uint8List;
    final k = keyBytes is Uint8List ? keyBytes : Uint8List.fromList(keyBytes);

    final bindings = LiliaNativeBindings.instance;
    final val = bindings.kvGet(_db._handle, namespace, k);
    if (val == null) return null;

    final keyStr = key is String ? key : base64Encode(k);
    _db._memoryKvCache['$namespace:$keyStr'] = utf8.decode(val);

    return KvEntry(
      namespace: namespace,
      key: k,
      value: val,
      version: 1,
    );
  }

  int set(String namespace, dynamic key, dynamic value, {int expiresAtMs = 0}) {
    _db._checkClosed();
    final keyBytes = key is String ? utf8.encode(key) : key as Uint8List;
    final valBytes = value is String ? utf8.encode(value) : value as Uint8List;
    final k = keyBytes is Uint8List ? keyBytes : Uint8List.fromList(keyBytes);
    final v = valBytes is Uint8List ? valBytes : Uint8List.fromList(valBytes);

    final keyStr = key is String ? key : base64Encode(k);
    _db._memoryKvCache['$namespace:$keyStr'] = utf8.decode(v);

    final version = LiliaNativeBindings.instance.kvSet(
      _db._handle,
      namespace,
      k,
      v,
      expiresAtMs: expiresAtMs,
    );

    _db.notifyChange(DatabaseKvChangeEvent(namespace: namespace, key: keyStr, op: 'set'));
    return version;
  }

  bool delete(String namespace, dynamic key) {
    _db._checkClosed();
    final keyBytes = key is String ? utf8.encode(key) : key as Uint8List;
    final k = keyBytes is Uint8List ? keyBytes : Uint8List.fromList(keyBytes);
    final keyStr = key is String ? key : base64Encode(k);

    _db._memoryKvCache.remove('$namespace:$keyStr');
    final deleted = LiliaNativeBindings.instance.kvDelete(_db._handle, namespace, k);

    _db.notifyChange(DatabaseKvChangeEvent(namespace: namespace, key: keyStr, op: 'delete'));
    return deleted;
  }
}

class _LiliaSyncJson {
  final LiliaDatabase _db;
  _LiliaSyncJson(this._db);

  JsonEntry<T>? get<T>(String space, String id) {
    _db._checkClosed();
    final jsonStr = LiliaNativeBindings.instance.jsonGet(_db._handle, space, id);
    if (jsonStr == null) return null;

    _db._memoryJsonCache.putIfAbsent(space, () => {})[id] = jsonStr;
    return JsonEntry<T>(
      space: space,
      id: id,
      value: jsonDecode(jsonStr) as T,
      version: 1,
    );
  }

  int put<T>(String space, String id, T value) {
    _db._checkClosed();
    final jsonStr = jsonEncode(value);
    _db._memoryJsonCache.putIfAbsent(space, () => {})[id] = jsonStr;

    final version = LiliaNativeBindings.instance.jsonPut(_db._handle, space, id, jsonStr);
    _db.notifyChange(DatabaseJsonChangeEvent(space: space, id: id, op: 'put'));
    return version;
  }

  bool delete(String space, String id) {
    _db._checkClosed();
    _db._memoryJsonCache[space]?.remove(id);

    final deleted = LiliaNativeBindings.instance.jsonDelete(_db._handle, space, id);
    _db.notifyChange(DatabaseJsonChangeEvent(space: space, id: id, op: 'delete'));
    return deleted;
  }

  List<JsonEntry<T>> find<T>(
    String space,
    Map<String, dynamic> query, {
    String? afterId,
    int limit = 50,
  }) {
    _db._checkClosed();
    final raw = LiliaNativeBindings.instance.jsonFind(
      _db._handle,
      space,
      jsonEncode(query),
      afterId: afterId,
      limit: limit,
    );

    final list = jsonDecode(raw) as List<dynamic>;
    return list.map((entry) {
      final map = entry as Map<String, dynamic>;
      return JsonEntry<T>(
        space: map['space'] as String? ?? space,
        id: map['id'] as String,
        value: map['value'] as T,
        version: map['version'] as int? ?? 1,
      );
    }).toList();
  }
}

class _LiliaSyncGraph {
  final LiliaDatabase _db;
  _LiliaSyncGraph(this._db);

  void setNode<T>(String graph, String id, String kind, T metadata) {
    _db._checkClosed();
    LiliaNativeBindings.instance.graphSetNode(
      _db._handle,
      graph,
      id,
      kind,
      jsonEncode(metadata),
    );
    _db.notifyChange(DatabaseGraphChangeEvent(graph: graph, op: 'node'));
  }

  GraphNode<T>? getNode<T>(String graph, String id) {
    _db._checkClosed();
    final raw = LiliaNativeBindings.instance.graphGetNode(_db._handle, graph, id);
    if (raw == null) return null;

    final map = jsonDecode(raw) as Map<String, dynamic>;
    return GraphNode<T>(
      graph: graph,
      id: id,
      kind: map['kind'] as String? ?? '',
      metadata: map['metadata'] as T,
    );
  }

  bool deleteNode(String graph, String id) {
    _db._checkClosed();
    final del = LiliaNativeBindings.instance.graphDeleteNode(_db._handle, graph, id);
    _db.notifyChange(DatabaseGraphChangeEvent(graph: graph, op: 'node'));
    return del;
  }

  void addEdge(String graph, String source, String target, String kind) {
    _db._checkClosed();
    LiliaNativeBindings.instance.graphAddEdge(_db._handle, graph, source, target, kind);
    _db.notifyChange(DatabaseGraphChangeEvent(graph: graph, op: 'edge'));
  }

  void removeEdge(String graph, String source, String target, String kind) {
    _db._checkClosed();
    LiliaNativeBindings.instance.graphRemoveEdge(_db._handle, graph, source, target, kind);
    _db.notifyChange(DatabaseGraphChangeEvent(graph: graph, op: 'edge'));
  }

  List<String> neighbors(
    String graph,
    String id, {
    String direction = 'out',
    String? kindFilter,
  }) {
    _db._checkClosed();
    return LiliaNativeBindings.instance.graphNeighbors(
      _db._handle,
      graph,
      id,
      direction: direction,
      kindFilter: kindFilter,
    );
  }

  bool reachability(String graph, String source, String target) {
    _db._checkClosed();
    return LiliaNativeBindings.instance.graphReachability(_db._handle, graph, source, target);
  }
}

class _LiliaSyncFts {
  final LiliaDatabase _db;
  _LiliaSyncFts(this._db);

  void index(String collection, String docId, String text) {
    _db._checkClosed();
    LiliaNativeBindings.instance.ftsIndex(_db._handle, collection, docId, text);
    _db.notifyChange(DatabaseFtsChangeEvent(collection: collection));
  }

  bool delete(String collection, String docId) {
    _db._checkClosed();
    final res = LiliaNativeBindings.instance.ftsDelete(_db._handle, collection, docId);
    _db.notifyChange(DatabaseFtsChangeEvent(collection: collection));
    return res;
  }

  List<FtsHit> search(String collection, String query, {int limit = 20}) {
    _db._checkClosed();
    final raw = LiliaNativeBindings.instance.ftsSearch(_db._handle, collection, query, limit: limit);
    final list = jsonDecode(raw) as List<dynamic>;

    return list.map((hit) {
      final map = hit as Map<String, dynamic>;
      return FtsHit(
        collection: map['collection'] as String? ?? collection,
        docId: map['doc_id'] as String? ?? map['docId'] as String? ?? '',
        snippet: map['snippet'] as String? ?? '',
        rank: (map['rank'] as num?)?.toDouble() ?? 0.0,
      );
    }).toList();
  }
}

class _LiliaSyncSql {
  final LiliaDatabase _db;
  _LiliaSyncSql(this._db);

  SqlQueryResult<T> query<T>(String sql, [List<dynamic>? params]) {
    _db._checkClosed();
    final raw = LiliaNativeBindings.instance.sqlQuery(_db._handle, sql, params);
    final map = jsonDecode(raw) as Map<String, dynamic>;

    final columns = (map['columns'] as List<dynamic>? ?? []).map((e) => e.toString()).toList();
    final rowsRaw = map['rows'] as List<dynamic>? ?? [];
    final rowsAffected = map['rows_affected'] as int? ?? map['rowsAffected'] as int? ?? 0;

    return SqlQueryResult<T>(
      columns: columns,
      rows: rowsRaw.map((r) => r as T).toList(),
      rowsAffected: rowsAffected,
    );
  }

  SqlQueryResult<T> mutate<T>(String sql, [List<dynamic>? params, List<String>? invalidateTables]) {
    final res = query<T>(sql, params);
    if (invalidateTables != null) {
      for (final table in invalidateTables) {
        _db.notifyChange(DatabaseTableChangeEvent(table: table));
      }
    }
    return res;
  }
}

class _LiliaSyncMaintenance {
  final LiliaDatabase _db;
  _LiliaSyncMaintenance(this._db);

  void checkpoint([String mode = 'truncate']) {
    _db._checkClosed();
    LiliaNativeBindings.instance.checkpoint(_db._handle, mode);
  }

  void vacuum() {
    _db._checkClosed();
    LiliaNativeBindings.instance.vacuum(_db._handle);
  }

  StorageMetrics getFileSize() {
    _db._checkClosed();
    final map = LiliaNativeBindings.instance.getFileSize(_db._handle);
    return StorageMetrics(
      mainBytes: map['main'] ?? 0,
      walBytes: map['wal'] ?? 0,
      shmBytes: map['shm'] ?? 0,
      totalBytes: map['total'] ?? 0,
    );
  }

  void backup(String destinationPath) {
    _db._checkClosed();
    LiliaNativeBindings.instance.backup(_db._handle, destinationPath);
  }
}

// --------------------------------------------------
// ASYNCHRONOUS SUB-ENGINE
// --------------------------------------------------

class _LiliaAsyncKv {
  final LiliaDatabase _db;
  _LiliaAsyncKv(this._db);

  Future<KvEntry?> get(String namespace, dynamic key) async => _db.sync.kv.get(namespace, key);
  Future<int> set(String namespace, dynamic key, dynamic value, {int expiresAtMs = 0}) async =>
      _db.sync.kv.set(namespace, key, value, expiresAtMs: expiresAtMs);
  Future<bool> delete(String namespace, dynamic key) async => _db.sync.kv.delete(namespace, key);
}

class _LiliaAsyncJson {
  final LiliaDatabase _db;
  _LiliaAsyncJson(this._db);

  Future<JsonEntry<T>?> get<T>(String space, String id) async => _db.sync.json.get<T>(space, id);
  Future<int> put<T>(String space, String id, T value) async => _db.sync.json.put<T>(space, id, value);
  Future<bool> delete(String space, String id) async => _db.sync.json.delete(space, id);
  Future<List<JsonEntry<T>>> find<T>(
    String space,
    Map<String, dynamic> query, {
    String? afterId,
    int limit = 50,
  }) async =>
      _db.sync.json.find<T>(space, query, afterId: afterId, limit: limit);
}

class _LiliaAsyncGraph {
  final LiliaDatabase _db;
  _LiliaAsyncGraph(this._db);

  Future<void> setNode<T>(String graph, String id, String kind, T metadata) async =>
      _db.sync.graph.setNode<T>(graph, id, kind, metadata);
  Future<GraphNode<T>?> getNode<T>(String graph, String id) async => _db.sync.graph.getNode<T>(graph, id);
  Future<bool> deleteNode(String graph, String id) async => _db.sync.graph.deleteNode(graph, id);
  Future<void> addEdge(String graph, String source, String target, String kind) async =>
      _db.sync.graph.addEdge(graph, source, target, kind);
  Future<void> removeEdge(String graph, String source, String target, String kind) async =>
      _db.sync.graph.removeEdge(graph, source, target, kind);
  Future<List<String>> neighbors(
    String graph,
    String id, {
    String direction = 'out',
    String? kindFilter,
  }) async =>
      _db.sync.graph.neighbors(graph, id, direction: direction, kindFilter: kindFilter);
  Future<bool> reachability(String graph, String source, String target) async =>
      _db.sync.graph.reachability(graph, source, target);
}

class _LiliaAsyncFts {
  final LiliaDatabase _db;
  _LiliaAsyncFts(this._db);

  Future<void> index(String collection, String docId, String text) async =>
      _db.sync.fts.index(collection, docId, text);
  Future<bool> delete(String collection, String docId) async => _db.sync.fts.delete(collection, docId);
  Future<List<FtsHit>> search(String collection, String query, {int limit = 20}) async =>
      _db.sync.fts.search(collection, query, limit: limit);
}

class _LiliaAsyncSql {
  final LiliaDatabase _db;
  _LiliaAsyncSql(this._db);

  Future<SqlQueryResult<T>> query<T>(String sql, [List<dynamic>? params]) async =>
      _db.sync.sql.query<T>(sql, params);
  Future<SqlQueryResult<T>> mutate<T>(String sql, [List<dynamic>? params, List<String>? invalidateTables]) async =>
      _db.sync.sql.mutate<T>(sql, params, invalidateTables);
}

class _LiliaAsyncMaintenance {
  final LiliaDatabase _db;
  _LiliaAsyncMaintenance(this._db);

  Future<void> checkpoint([String mode = 'truncate']) async => _db.sync.maintenance.checkpoint(mode);
  Future<void> vacuum() async => _db.sync.maintenance.vacuum();
  Future<StorageMetrics> getFileSize() async => _db.sync.maintenance.getFileSize();
  Future<void> backup(String destinationPath) async => _db.sync.maintenance.backup(destinationPath);
}
