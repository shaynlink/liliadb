import React, { useMemo } from 'react';
import {
  View,
  Text,
  StyleSheet,
  type StyleProp,
  type ViewStyle,
  type TextStyle,
} from 'react-native';
import { useSqlQuery } from '../../hooks.js';
import type { LiliaDatabase } from '../../database.js';

export interface LiliaCountBadgeProps {
  sql?: string;
  sqlParams?: unknown[];
  tables?: string[];
  count?: number;
  maxDisplay?: number;
  hideZero?: boolean;
  badgeStyle?: StyleProp<ViewStyle>;
  textStyle?: StyleProp<TextStyle>;
  db?: LiliaDatabase | null;
}

/**
 * Idiomatic numeric count pill badge (e.g. for notification unread counts or pending items)
 * connected directly to a live LiliaDB count query.
 */
export function LiliaCountBadge({
  sql,
  sqlParams,
  tables,
  count: staticCount,
  maxDisplay = 99,
  hideZero = false,
  badgeStyle,
  textStyle,
  db,
}: LiliaCountBadgeProps): React.ReactElement | null {
  const queryResult = useSqlQuery<Record<string, unknown>>(sql ?? '', sqlParams, {
    tables,
    enabled: Boolean(sql),
    db,
  });

  const resolvedCount = useMemo(() => {
    if (sql) {
      if (queryResult.data.length > 0) {
        const row = queryResult.data[0];
        const val = Object.values(row)[0];
        if (typeof val === 'number') return val;
        if (typeof val === 'string') {
          const parsed = parseInt(val, 10);
          return isNaN(parsed) ? 0 : parsed;
        }
      }
      return 0;
    }
    return staticCount ?? 0;
  }, [sql, queryResult.data, staticCount]);

  if (hideZero && resolvedCount <= 0) {
    return null;
  }

  const displayText =
    resolvedCount > maxDisplay ? `${maxDisplay}+` : String(resolvedCount);

  return (
    <View style={[styles.badge, badgeStyle]}>
      <Text style={[styles.text, textStyle]}>{displayText}</Text>
    </View>
  );
}

const styles = StyleSheet.create({
  badge: {
    minWidth: 20,
    height: 20,
    borderRadius: 10,
    backgroundColor: '#EF4444',
    justifyContent: 'center',
    alignItems: 'center',
    paddingHorizontal: 6,
  },
  text: {
    color: '#FFFFFF',
    fontSize: 12,
    fontWeight: '700',
    textAlign: 'center',
  },
});
