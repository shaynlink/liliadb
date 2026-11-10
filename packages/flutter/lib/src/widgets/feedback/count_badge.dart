import 'package:flutter/material.dart';
import '../../builders/query_builder.dart';
import '../../database.dart';

/// Live count pill badge (e.g. for notifications or unread messages) bound directly to a SQL query.
class LiliaCountBadge extends StatelessWidget {
  final String? sql;
  final List<dynamic>? params;
  final List<String>? tables;
  final int? count;
  final int maxDisplay;
  final bool hideZero;
  final Color backgroundColor;
  final Color textColor;
  final LiliaDatabase? database;

  const LiliaCountBadge({
    super.key,
    this.sql,
    this.params,
    this.tables,
    this.count,
    this.maxDisplay = 99,
    this.hideZero = false,
    this.backgroundColor = const Color(0xFFEF4444),
    this.textColor = Colors.white,
    this.database,
  });

  @override
  Widget build(BuildContext context) {
    if (sql != null) {
      return LiliaQueryBuilder<Map<String, dynamic>>(
        sql: sql!,
        params: params,
        tables: tables,
        database: database,
        builder: (context, result, isLoading, error, refetch) {
          int resolvedCount = 0;
          if (result != null && result.rows.isNotEmpty) {
            final firstVal = result.rows.first.values.first;
            if (firstVal is int) {
              resolvedCount = firstVal;
            } else if (firstVal is num) {
              resolvedCount = firstVal.toInt();
            } else if (firstVal is String) {
              resolvedCount = int.tryParse(firstVal) ?? 0;
            }
          }
          return _buildPill(resolvedCount);
        },
      );
    }

    return _buildPill(count ?? 0);
  }

  Widget _buildPill(int countVal) {
    if (hideZero && countVal <= 0) {
      return const SizedBox.shrink();
    }

    final displayText = countVal > maxDisplay ? '$maxDisplay+' : '$countVal';

    return Container(
      constraints: const BoxConstraints(minWidth: 20, minHeight: 20),
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
      decoration: BoxDecoration(
        color: backgroundColor,
        borderRadius: BorderRadius.circular(10),
      ),
      child: Center(
        child: Text(
          displayText,
          style: TextStyle(
            color: textColor,
            fontSize: 11,
            fontWeight: FontWeight.bold,
          ),
        ),
      ),
    );
  }
}
