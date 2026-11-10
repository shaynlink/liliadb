import 'package:flutter/material.dart';
import '../../builders/document_builder.dart';
import '../../database.dart';

typedef LiliaDocumentViewBuilder<T> = Widget Function(
  BuildContext context,
  T? doc,
  Future<void> Function(T newDoc) update,
  Future<void> Function() delete,
);

/// Render-prop builder widget observing a single JSON document in LiliaDB.
///
/// Isolates re-renders strictly to this widget's subtree.
class LiliaDocumentView<T> extends StatelessWidget {
  final String space;
  final String id;
  final T? defaultValue;
  final Widget? loadingWidget;
  final Widget Function(Object error)? errorBuilder;
  final LiliaDocumentViewBuilder<T> builder;
  final LiliaDatabase? database;

  const LiliaDocumentView({
    super.key,
    required this.space,
    required this.id,
    this.defaultValue,
    this.loadingWidget,
    this.errorBuilder,
    required this.builder,
    this.database,
  });

  @override
  Widget build(BuildContext context) {
    return LiliaDocumentBuilder<T>(
      space: space,
      id: id,
      defaultValue: defaultValue,
      database: database,
      builder: (context, doc, isLoading, error, update, delete) {
        if (isLoading && loadingWidget != null) {
          return loadingWidget!;
        }

        if (error != null) {
          if (errorBuilder != null) return errorBuilder!(error);
          return Center(child: Text('Document error: $error'));
        }

        return builder(context, doc, update, delete);
      },
    );
  }
}
