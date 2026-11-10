import 'dart:convert';
import 'package:flutter/widgets.dart';
import '../database.dart';
import '../scope.dart';

typedef LiliaValueWidgetBuilder<T> = Widget Function(
  BuildContext context,
  T value,
  void Function(T newValue) setValue,
);

/// Reactive widget listening to live updates of a Key-Value pair in LiliaDB.
///
/// Features Zero-FOUC (synchronous initial value reading without flashing loading states).
class LiliaValueBuilder<T> extends StatefulWidget {
  final String namespace;
  final String keyName;
  final T defaultValue;
  final LiliaDatabase? database;
  final LiliaValueWidgetBuilder<T> builder;

  const LiliaValueBuilder({
    super.key,
    required this.namespace,
    required this.keyName,
    required this.defaultValue,
    this.database,
    required this.builder,
  });

  @override
  State<LiliaValueBuilder<T>> createState() => _LiliaValueBuilderState<T>();
}

class _LiliaValueBuilderState<T> extends State<LiliaValueBuilder<T>> {
  late T _value;
  void Function()? _unsubscribe;
  LiliaDatabase? _db;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final db = widget.database ?? LiliaScope.of(context);
    if (_db != db) {
      _db = db;
      _subscribe();
    }
  }

  void _subscribe() {
    _unsubscribe?.call();
    if (_db == null) return;

    // Zero-FOUC synchronous initial value
    _readInitialValue();

    final pattern = 'kv:${widget.namespace}:${widget.keyName}';
    _unsubscribe = _db!.subscribe(pattern, () {
      if (mounted) {
        setState(() {
          _readInitialValue();
        });
      }
    });
  }

  void _readInitialValue() {
    final peeked = _db!.sync.kv.peek(widget.namespace, widget.keyName);
    if (peeked == null) {
      _value = widget.defaultValue;
    } else {
      try {
        if (T == bool) {
          _value = (peeked == 'true' || peeked == '1') as T;
        } else if (T == int) {
          _value = (int.tryParse(peeked) ?? widget.defaultValue) as T;
        } else if (T == double) {
          _value = (double.tryParse(peeked) ?? widget.defaultValue) as T;
        } else if (T == String) {
          _value = peeked as T;
        } else {
          _value = jsonDecode(peeked) as T;
        }
      } catch (_) {
        _value = widget.defaultValue;
      }
    }
  }

  void _setValue(T newValue) {
    if (_db == null) return;
    final strVal = (newValue is String) ? newValue : jsonEncode(newValue);
    _db!.sync.kv.set(widget.namespace, widget.keyName, strVal);
    setState(() {
      _value = newValue;
    });
  }

  @override
  void dispose() {
    _unsubscribe?.call();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return widget.builder(context, _value, _setValue);
  }
}
