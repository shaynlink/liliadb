import 'package:flutter/material.dart';
import '../../builders/query_builder.dart';
import '../../database.dart';

typedef LiliaListItemBuilder<T> = Widget Function(
  BuildContext context,
  T item,
  int index,
);

/// High-performance virtualized ListView connected live to a LiliaDB SQL query.
class LiliaListView<T> extends StatelessWidget {
  final String sql;
  final List<dynamic>? params;
  final List<String>? tables;
  final LiliaListItemBuilder<T> itemBuilder;
  final Widget? loadingWidget;
  final Widget? emptyWidget;
  final Widget Function(Object error)? errorBuilder;
  final ScrollPhysics? physics;
  final EdgeInsetsGeometry? padding;
  final LiliaDatabase? database;

  const LiliaListView({
    super.key,
    required this.sql,
    this.params,
    this.tables,
    required this.itemBuilder,
    this.loadingWidget,
    this.emptyWidget,
    this.errorBuilder,
    this.physics,
    this.padding,
    this.database,
  });

  @override
  Widget build(BuildContext context) {
    return LiliaQueryBuilder<T>(
      sql: sql,
      params: params,
      tables: tables,
      database: database,
      builder: (context, result, isLoading, error, refetch) {
        if (isLoading && loadingWidget != null) {
          return loadingWidget!;
        }

        if (error != null) {
          if (errorBuilder != null) return errorBuilder!(error);
          return Center(child: Text('Query error: $error'));
        }

        final items = result?.rows ?? <T>[];

        if (items.isEmpty && emptyWidget != null) {
          return emptyWidget!;
        }

        return RefreshIndicator(
          onRefresh: () async => refetch(),
          child: ListView.builder(
            physics: physics,
            padding: padding,
            itemCount: items.length,
            itemBuilder: (context, index) {
              return itemBuilder(context, items[index], index);
            },
          ),
        );
      },
    );
  }
}
