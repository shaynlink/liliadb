sealed class DatabaseChangeEvent {
  const DatabaseChangeEvent();
}

class DatabaseKvChangeEvent extends DatabaseChangeEvent {
  final String namespace;
  final String key;
  final String op; // 'set' | 'delete'

  const DatabaseKvChangeEvent({
    required this.namespace,
    required this.key,
    required this.op,
  });

  @override
  String toString() => 'DatabaseKvChangeEvent($namespace:$key, $op)';
}

class DatabaseJsonChangeEvent extends DatabaseChangeEvent {
  final String space;
  final String id;
  final String op; // 'put' | 'delete'

  const DatabaseJsonChangeEvent({
    required this.space,
    required this.id,
    required this.op,
  });

  @override
  String toString() => 'DatabaseJsonChangeEvent($space:$id, $op)';
}

class DatabaseGraphChangeEvent extends DatabaseChangeEvent {
  final String graph;
  final String op; // 'node' | 'edge'

  const DatabaseGraphChangeEvent({
    required this.graph,
    required this.op,
  });

  @override
  String toString() => 'DatabaseGraphChangeEvent($graph, $op)';
}

class DatabaseFtsChangeEvent extends DatabaseChangeEvent {
  final String collection;

  const DatabaseFtsChangeEvent({
    required this.collection,
  });

  @override
  String toString() => 'DatabaseFtsChangeEvent($collection)';
}

class DatabaseTableChangeEvent extends DatabaseChangeEvent {
  final String table;

  const DatabaseTableChangeEvent({
    required this.table,
  });

  @override
  String toString() => 'DatabaseTableChangeEvent($table)';
}

class DatabaseCustomChangeEvent extends DatabaseChangeEvent {
  final String tag;

  const DatabaseCustomChangeEvent({
    required this.tag,
  });

  @override
  String toString() => 'DatabaseCustomChangeEvent($tag)';
}

/// Pub/Sub reactive observer for LiliaDB in Flutter.
class DatabaseObserver {
  final Map<int, ({String pattern, void Function() callback})> _listeners = {};
  int _nextId = 1;

  /// Subscribes to database change events matching [pattern].
  ///
  /// Examples of supported patterns:
  /// - `*` (any event)
  /// - `kv:app_settings` (any change in `app_settings` namespace)
  /// - `kv:app_settings:theme` (specific key)
  /// - `json:users` (any change in `users` space)
  /// - `json:users:me` (specific document)
  /// - `table:activity` (SQL table changed)
  /// - `graph:social` (graph mutation)
  /// - `fts:documents` (FTS index update)
  void Function() subscribe(String pattern, void Function() callback) {
    final id = _nextId++;
    _listeners[id] = (pattern: pattern, callback: callback);

    return () {
      _listeners.remove(id);
    };
  }

  void emit(DatabaseChangeEvent event) {
    final callbacksToRun = <void Function()>[];

    for (final listener in _listeners.values) {
      if (_matches(listener.pattern, event)) {
        callbacksToRun.add(listener.callback);
      }
    }

    for (final cb in callbacksToRun) {
      try {
        cb();
      } catch (_) {}
    }
  }

  void clear() {
    _listeners.clear();
  }

  bool _matches(String pattern, DatabaseChangeEvent event) {
    if (pattern == '*' || pattern.isEmpty) return true;

    return switch (event) {
      DatabaseKvChangeEvent(namespace: final ns, key: final k) =>
        pattern == 'kv' ||
        pattern == 'kv:$ns' ||
        pattern == 'kv:$ns:$k' ||
        pattern == 'kv:*' ||
        pattern == 'kv:$ns:*',
      DatabaseJsonChangeEvent(space: final sp, id: final id) =>
        pattern == 'json' ||
        pattern == 'json:$sp' ||
        pattern == 'json:$sp:$id' ||
        pattern == 'json:*' ||
        pattern == 'json:$sp:*',
      DatabaseGraphChangeEvent(graph: final g) =>
        pattern == 'graph' || pattern == 'graph:$g' || pattern == 'graph:*',
      DatabaseFtsChangeEvent(collection: final c) =>
        pattern == 'fts' || pattern == 'fts:$c' || pattern == 'fts:*',
      DatabaseTableChangeEvent(table: final t) =>
        pattern == 'table' || pattern == 'table:$t' || pattern == 'table:*',
      DatabaseCustomChangeEvent(tag: final tag) =>
        pattern == 'custom' || pattern == 'custom:$tag' || pattern == 'custom:*',
    };
  }
}
