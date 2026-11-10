import 'package:flutter/widgets.dart';
import '../database.dart';
import '../scope.dart';

typedef LiliaDocumentWidgetBuilder<T> = Widget Function(
  BuildContext context,
  T? doc,
  bool isLoading,
  Object? error,
  Future<void> Function(T newDoc) update,
  Future<void> Function() delete,
);

/// Reactive widget observing a single JSON document in LiliaDB.
class LiliaDocumentBuilder<T> extends StatefulWidget {
  final String space;
  final String id;
  final T? defaultValue;
  final LiliaDatabase? database;
  final LiliaDocumentWidgetBuilder<T> builder;

  const LiliaDocumentBuilder({
    super.key,
    required this.space,
    required this.id,
    this.defaultValue,
    this.database,
    required this.builder,
  });

  @override
  State<LiliaDocumentBuilder<T>> createState() => _LiliaDocumentBuilderState<T>();
}

class _LiliaDocumentBuilderState<T> extends State<LiliaDocumentBuilder<T>> {
  T? _doc;
  bool _isLoading = true;
  Object? _error;
  void Function()? _unsubscribe;
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

  void _initAndSubscribe() {
    _unsubscribe?.call();
    if (_db == null) return;

    _readDoc();

    final pattern = 'json:${widget.space}:${widget.id}';
    _unsubscribe = _db!.subscribe(pattern, () {
      if (mounted) _readDoc();
    });
  }

  void _readDoc() {
    try {
      final entry = _db!.sync.json.get<T>(widget.space, widget.id);
      setState(() {
        _doc = entry?.value ?? widget.defaultValue;
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

  Future<void> _update(T newDoc) async {
    if (_db == null) return;
    _db!.sync.json.put<T>(widget.space, widget.id, newDoc);
    setState(() {
      _doc = newDoc;
    });
  }

  Future<void> _delete() async {
    if (_db == null) return;
    _db!.sync.json.delete(widget.space, widget.id);
    setState(() {
      _doc = null;
    });
  }

  @override
  void dispose() {
    _unsubscribe?.call();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return widget.builder(context, _doc, _isLoading, _error, _update, _delete);
  }
}
