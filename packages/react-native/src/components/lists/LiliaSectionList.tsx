import React, { useMemo } from 'react';
import {
  SectionList,
  RefreshControl,
  type SectionListProps,
  type SectionListData,
  View,
} from 'react-native';
import { useSqlQuery } from '../../hooks.js';
import type { LiliaDatabase } from '../../database.js';

export interface LiliaSectionData<T> {
  title: string;
  data: T[];
  [key: string]: any;
}

export interface LiliaSectionListProps<T = Record<string, unknown>>
  extends Omit<SectionListProps<T, LiliaSectionData<T>>, 'sections'> {
  sql: string;
  sqlParams?: unknown[];
  tables?: string[];
  groupBy: (item: T) => string;
  sortSections?: (a: string, b: string) => number;
  loadingComponent?: React.ReactNode;
  emptyComponent?: React.ReactNode;
  errorComponent?: (error: Error) => React.ReactNode;
  db?: LiliaDatabase | null;
}

/**
 * Idiomatic React Native SectionList automatically transforming flat SQL query rows
 * into grouped sections with sticky header support.
 */
export function LiliaSectionList<T = Record<string, unknown>>({
  sql,
  sqlParams,
  tables,
  groupBy,
  sortSections,
  renderItem,
  renderSectionHeader,
  keyExtractor,
  loadingComponent,
  emptyComponent,
  errorComponent,
  db,
  ...restProps
}: LiliaSectionListProps<T>): React.ReactElement {
  const queryResult = useSqlQuery<T>(sql, sqlParams, {
    tables,
    enabled: Boolean(sql),
    db,
  });

  const sections: LiliaSectionData<T>[] = useMemo(() => {
    if (!queryResult.data || queryResult.data.length === 0) {
      return [];
    }

    const groups = new Map<string, T[]>();
    for (const item of queryResult.data) {
      const groupKey = groupBy(item) || 'Other';
      const existing = groups.get(groupKey);
      if (existing) {
        existing.push(item);
      } else {
        groups.set(groupKey, [item]);
      }
    }

    const groupKeys = Array.from(groups.keys());
    if (sortSections) {
      groupKeys.sort(sortSections);
    }

    return groupKeys.map((title) => ({
      title,
      data: groups.get(title) ?? [],
    }));
  }, [queryResult.data, groupBy, sortSections]);

  if (queryResult.isLoading && loadingComponent) {
    return <>{loadingComponent}</>;
  }

  if (queryResult.error && errorComponent) {
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

  const refreshControl = (
    <RefreshControl
      refreshing={queryResult.isRefetching}
      onRefresh={queryResult.refetch}
    />
  );

  return (
    <SectionList<T, LiliaSectionData<T>>
      sections={sections}
      renderItem={renderItem}
      renderSectionHeader={renderSectionHeader}
      keyExtractor={defaultKeyExtractor}
      ListEmptyComponent={
        emptyComponent ? () => <>{emptyComponent}</> : restProps.ListEmptyComponent
      }
      refreshControl={refreshControl}
      {...restProps}
    />
  );
}
