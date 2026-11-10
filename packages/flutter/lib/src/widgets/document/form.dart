import 'dart:convert';
import 'package:flutter/material.dart';
import '../../builders/document_builder.dart';
import '../../database.dart';

class LiliaFormController<T> {
  final T values;
  final void Function(T newValues) setValues;
  final bool isDirty;
  final bool isSubmitting;
  final Future<void> Function() submit;
  final void Function() reset;

  const LiliaFormController({
    required this.values,
    required this.setValues,
    required this.isDirty,
    required this.isSubmitting,
    required this.submit,
    required this.reset,
  });
}

typedef LiliaFormWidgetBuilder<T> = Widget Function(
  BuildContext context,
  LiliaFormController<T> form,
);

/// Declarative form container bound directly to a LiliaDB JSON document.
///
/// Tracks modifications, manages `isDirty` state, and commits atomic updates.
class LiliaForm<T> extends StatefulWidget {
  final String space;
  final String id;
  final T initialValues;
  final Future<void> Function(T values)? onSubmit;
  final void Function(T values)? onSuccess;
  final void Function(Object error)? onError;
  final LiliaFormWidgetBuilder<T> builder;
  final LiliaDatabase? database;

  const LiliaForm({
    super.key,
    required this.space,
    required this.id,
    required this.initialValues,
    this.onSubmit,
    this.onSuccess,
    this.onError,
    required this.builder,
    this.database,
  });

  @override
  State<LiliaForm<T>> createState() => _LiliaFormState<T>();
}

class _LiliaFormState<T> extends State<LiliaForm<T>> {
  late T _currentValues;
  late T _baseValues;
  bool _isSubmitting = false;

  @override
  void initState() {
    super.initState();
    _currentValues = widget.initialValues;
    _baseValues = widget.initialValues;
  }

  bool get _isDirty {
    try {
      return jsonEncode(_currentValues) != jsonEncode(_baseValues);
    } catch (_) {
      return _currentValues != _baseValues;
    }
  }

  void _setValues(T newValues) {
    setState(() {
      _currentValues = newValues;
    });
  }

  void _reset() {
    setState(() {
      _currentValues = _baseValues;
    });
  }

  @override
  Widget build(BuildContext context) {
    return LiliaDocumentBuilder<T>(
      space: widget.space,
      id: widget.id,
      defaultValue: widget.initialValues,
      database: widget.database,
      builder: (context, doc, isLoading, error, update, delete) {
        if (doc != null && jsonEncode(doc) != jsonEncode(_baseValues)) {
          _baseValues = doc;
          _currentValues = doc;
        }

        Future<void> handleSubmit() async {
          setState(() {
            _isSubmitting = true;
          });
          try {
            if (widget.onSubmit != null) {
              await widget.onSubmit!(_currentValues);
            }
            await update(_currentValues);
            _baseValues = _currentValues;
            widget.onSuccess?.(_currentValues);
          } catch (e) {
            widget.onError?.(e);
          } finally {
            if (mounted) {
              setState(() {
                _isSubmitting = false;
              });
            }
          }
        }

        final controller = LiliaFormController<T>(
          values: _currentValues,
          setValues: _setValues,
          isDirty: _isDirty,
          isSubmitting: _isSubmitting,
          submit: handleSubmit,
          reset: _reset,
        );

        return widget.builder(context, controller);
      },
    );
  }
}
