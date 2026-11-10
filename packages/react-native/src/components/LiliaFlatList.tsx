import React, { useMemo } from 'react';
import { FlatList, RefreshControl, View } from 'react-native';
import { useSqlQuery } from '../hooks.js';
import type { LiliaFlatListProps } from '../types.js';

/**
 * Idiomatic React Native FlatList directly connected to LiliaDB.
 *
 * Automatically:
 * - Executes the provided SQL query
 * - Subscribes to table change events and re-renders live without polling
 * - Binds pull-to-refresh to query refetching
 * - Renders loading, error, and empty states
 */
export function LiliaFlatList<T = Record<string, unknown>>({
  sql,
  sqlParams,
  tables,
  data: staticData,
  renderItem,
  keyExtractor,
  loadingComponent,
  emptyComponent,
  errorComponent,
  db,
  ...flatListProps
}: LiliaFlatListProps<T>): React.ReactElement {
  const queryResult = useSqlQuery<T>(
    sql ?? '',
    sqlParams,
    {
      tables,
      enabled: Boolean(sql),
      db,
    }
  );

  const items: T[] = useMemo(() => {
    if (sql) return queryResult.data;
    return staticData ?? [];
  }, [sql, queryResult.data, staticData]);

  if (sql && queryResult.isLoading && loadingComponent) {
    return <>{loadingComponent}</>;
  }

  if (sql && queryResult.error && errorComponent) {
    return <>{errorComponent(queryResult.error)}</>;
  }

  const defaultKeyExtractor = (item: T, index: number): string => {
    if (keyExtractor) return keyExtractor(item, index);
    if (item && typeof item === 'object') {
      const anyObj = item as Record<string, unknown>;
      if ('id' in anyObj && anyObj.id != null) return String(anyObj.id);
      if ('rowid' in anyObj && anyObj.rowid != null) return String(anyObj.rowid);
      if ('key' in anyObj && anyObj.key != null) return String(anyObj.key);
    }
    return String(index);
  };

  const refreshControl = sql ? (
    <RefreshControl
      refreshing={queryResult.isRefetching}
      onRefresh={queryResult.refetch}
    />
  ) : (
    flatListProps.refreshControl
  );

  return (
    <FlatList<T>
      data={items}
      renderItem={renderItem}
      keyExtractor={defaultKeyExtractor}
      ListEmptyComponent={emptyComponent ? () => <>{emptyComponent}</> : flatListProps.ListEmptyComponent}
      refreshControl={refreshControl}
      {...flatListProps}
    />
  );
}
