import 'package:flutter/material.dart';
import '../../builders/query_builder.dart';
import '../../database.dart';

typedef LiliaSectionHeaderBuilder = Widget Function(
  BuildContext context,
  String sectionTitle,
);

typedef LiliaSectionItemBuilder<T> = Widget Function(
  BuildContext context,
  T item,
  int index,
);

/// Groups flat SQL query results into structured sections with sticky or separated headers.
class LiliaSectionListView<T> extends StatelessWidget {
  final String sql;
  final List<dynamic>? params;
  final List<String>? tables;
  final String Function(T item) groupBy;
  final int Function(String a, String b)? sortSections;
  final LiliaSectionHeaderBuilder sectionHeaderBuilder;
  final LiliaSectionItemBuilder<T> itemBuilder;
  final Widget? loadingWidget;
  final Widget? emptyWidget;
  final Widget Function(Object error)? errorBuilder;
  final EdgeInsetsGeometry? padding;
  final LiliaDatabase? database;

  const LiliaSectionListView({
    super.key,
    required this.sql,
    this.params,
    this.tables,
    required this.groupBy,
    this.sortSections,
    required this.sectionHeaderBuilder,
    required this.itemBuilder,
    this.loadingWidget,
    this.emptyWidget,
    this.errorBuilder,
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
          return Center(child: Text('Section query error: $error'));
        }

        final items = result?.rows ?? <T>[];

        if (items.isEmpty && emptyWidget != null) {
          return emptyWidget!;
        }

        final groups = <String, List<T>>{};
        for (final item in items) {
          final title = groupBy(item);
          groups.putIfAbsent(title, () => []).add(item);
        }

        final sortedTitles = groups.keys.toList();
        if (sortSections != null) {
          sortedTitles.sort(sortSections);
        }

        final slivers = <Widget>[];
        for (final title in sortedTitles) {
          slivers.add(
            SliverToBoxAdapter(
              child: sectionHeaderBuilder(context, title),
            ),
          );

          final sectionItems = groups[title]!;
          slivers.add(
            SliverList(
              delegate: SliverChildBuilderDelegate(
                (context, index) => itemBuilder(context, sectionItems[index], index),
                childCount: sectionItems.length,
              ),
            ),
          );
        }

        return RefreshIndicator(
          onRefresh: () async => refetch(),
          child: CustomScrollView(
            slivers: slivers,
          ),
        );
      },
    );
  }
}
