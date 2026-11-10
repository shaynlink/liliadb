import 'dart:async';
import 'package:flutter/material.dart';
import '../../database.dart';
import '../../scope.dart';

/// Text input field with automatic debounced persistence into LiliaDB Key-Value.
///
/// Ideal for persistent draft forms, note fields, and auto-saving text.
class LiliaKvTextField extends StatefulWidget {
  final String namespace;
  final String keyName;
  final String defaultValue;
  final int debounceMs;
  final InputDecoration? decoration;
  final TextStyle? style;
  final int? maxLines;
  final ValueChanged<String>? onChanged;
  final LiliaDatabase? database;

  const LiliaKvTextField({
    super.key,
    required this.namespace,
    required this.keyName,
    this.defaultValue = '',
    this.debounceMs = 300,
    this.decoration,
    this.style,
    this.maxLines = 1,
    this.onChanged,
    this.database,
  });

  @override
  State<LiliaKvTextField> createState() => _LiliaKvTextFieldState();
}

class _LiliaKvTextFieldState extends State<LiliaKvTextField> {
  late final TextEditingController _controller;
  Timer? _debounceTimer;
  LiliaDatabase? _db;
  bool _isEditing = false;

  @override
  void initState() {
    super.initState();
    _controller = TextEditingController();
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final db = widget.database ?? LiliaScope.of(context);
    if (_db != db) {
      _db = db;
      _initFromStorage();
    }
  }

  void _initFromStorage() {
    if (_db == null || _isEditing) return;
    final peeked = _db!.sync.kv.peek(widget.namespace, widget.keyName);
    final val = peeked ?? widget.defaultValue;
    if (_controller.text != val) {
      _controller.text = val;
    }
  }

  void _onTextChanged(String text) {
    _isEditing = true;
    widget.onChanged?.(text);

    _debounceTimer?.cancel();
    _debounceTimer = Timer(Duration(milliseconds: widget.debounceMs), () {
      if (_db != null) {
        _db!.sync.kv.set(widget.namespace, widget.keyName, text);
        _isEditing = false;
      }
    });
  }

  @override
  void dispose() {
    _debounceTimer?.cancel();
    // Flush on dispose
    if (_isEditing && _db != null) {
      _db!.sync.kv.set(widget.namespace, widget.keyName, _controller.text);
    }
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return TextField(
      controller: _controller,
      decoration: widget.decoration,
      style: widget.style,
      maxLines: widget.maxLines,
      onChanged: _onTextChanged,
    );
  }
}
