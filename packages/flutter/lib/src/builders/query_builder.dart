import 'package:flutter/widgets.dart';
import '../database.dart';
import '../scope.dart';

typedef LiliaQueryWidgetBuilder<T> = Widget Function(
  BuildContext context,
  SqlQueryResult<T>? result,
  bool isLoading,
  Object? error,
  void Function() refetch,
);

/// Reactive widget executing a SQL query on LiliaDB and auto-updating when watched tables mutate.
class LiliaQueryBuilder<T> extends StatefulWidget {
  final String sql;
  final List<dynamic>? params;
  final List<String>? tables;
  final LiliaDatabase? database;
  final LiliaQueryWidgetBuilder<T> builder;

  const LiliaQueryBuilder({
    super.key,
    required this.sql,
    this.params,
    this.tables,
    this.database,
    required this.builder,
  });

  @override
  State<LiliaQueryBuilder<T>> createState() => _LiliaQueryBuilderState<T>();
}

class _LiliaQueryBuilderState<T> extends State<LiliaQueryBuilder<T>> {
  SqlQueryResult<T>? _result;
  bool _isLoading = true;
  Object? _error;
  final List<void Function()> _unsubscribers = [];
  LiliaDatabase? _db;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final db = widget.database ?? LiliaScope.of(context);
    if (_db != db) {
      _db = db;
      _initAndSubscribe();
    }
  }

  @override
  void didUpdateWidget(LiliaQueryBuilder<T> oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (widget.sql != oldWidget.sql || widget.database != oldWidget.database) {
      _initAndSubscribe();
    }
  }

  void _initAndSubscribe() {
    _clearSubscriptions();
    if (_db == null) return;

    // Synchronous execution for 0ms initial render
    _executeQuery();

    if (widget.tables != null) {
      for (final table in widget.tables!) {
        final unsub = _db!.subscribe('table:$table', () {
          if (mounted) _executeQuery();
        });
        _unsubscribers.add(unsub);
      }
    }
  }

  void _executeQuery() {
    if (_db == null) return;
    try {
      final res = _db!.sync.sql.query<T>(widget.sql, widget.params);
      setState(() {
        _result = res;
        _isLoading = false;
        _error = null;
      });
    } catch (e) {
      setState(() {
        _isLoading = false;
        _error = e;
      });
    }
  }

  void _clearSubscriptions() {
    for (final unsub in _unsubscribers) {
      unsub();
    }
    _unsubscribers.clear();
  }

  @override
  void dispose() {
    _clearSubscriptions();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return widget.builder(context, _result, _isLoading, _error, _executeQuery);
  }
}
