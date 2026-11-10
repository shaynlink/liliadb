import React, { useState, useEffect, useCallback, useRef } from 'react';
import {
  FlatList,
  RefreshControl,
  ActivityIndicator,
  View,
  type FlatListProps,
  type ListRenderItem,
} from 'react-native';
import { getDefaultDatabase, type LiliaDatabase } from '../../database.js';
import { useDatabase } from '../../context.js';
import type { JsonEntry } from '../../types.js';

export interface LiliaInfiniteListProps<T = unknown>
  extends Omit<FlatListProps<JsonEntry<T>>, 'data' | 'renderItem'> {
  space: string;
  pageSize?: number;
  filter?: Record<string, unknown>;
  renderItem: ListRenderItem<JsonEntry<T>>;
  loadingComponent?: React.ReactNode;
  emptyComponent?: React.ReactNode;
  errorComponent?: (error: Error) => React.ReactNode;
  db?: LiliaDatabase | null;
}

/**
 * Idiomatic React Native infinite scrolling list backed by LiliaDB JSON space cursor pagination.
 */
export function LiliaInfiniteList<T = unknown>({
  space,
  pageSize = 20,
  filter = {},
  renderItem,
  keyExtractor,
  loadingComponent,
  emptyComponent,
  errorComponent,
  db: explicitDb,
  onEndReachedThreshold = 0.5,
  ...flatListProps
}: LiliaInfiniteListProps<T>): React.ReactElement {
  const [items, setItems] = useState<JsonEntry<T>[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [isLoadingMore, setIsLoadingMore] = useState(false);
  const [hasMore, setHasMore] = useState(true);
  const [error, setError] = useState<Error | null>(null);

  const lastIdRef = useRef<string | undefined>(undefined);
  const isLoadingRef = useRef(false);

  let resolvedDb: LiliaDatabase | null = null;
  try {
    resolvedDb = useDatabase(explicitDb);
  } catch {
    resolvedDb = explicitDb ?? getDefaultDatabase();
  }

  const fetchPage = useCallback(
    async (isRefresh = false) => {
      if (isLoadingRef.current) return;
      isLoadingRef.current = true;

      const db = resolvedDb;
      if (!db) {
        isLoadingRef.current = false;
        return;
      }

      try {
        const afterId = isRefresh ? undefined : lastIdRef.current;
        const results = await db.json.find<T>(space, filter, afterId, pageSize);

        if (isRefresh) {
          setItems(results);
        } else {
          setItems((prev) => [...prev, ...results]);
        }

        if (results.length > 0) {
          lastIdRef.current = results[results.length - 1].id;
        }

        setHasMore(results.length >= pageSize);
        setError(null);
      } catch (err) {
        setError(err instanceof Error ? err : new Error(String(err)));
      } finally {
        isLoadingRef.current = false;
        setIsLoading(false);
        setIsRefreshing(false);
        setIsLoadingMore(false);
      }
    },
    [space, filter, pageSize, resolvedDb]
  );

  useEffect(() => {
    setIsLoading(true);
    lastIdRef.current = undefined;
    fetchPage(true);
  }, [fetchPage]);

  // Listen for changes in the JSON space
  useEffect(() => {
    const db = resolvedDb;
    if (!db) return;

    const unsubscribe = db.subscribe(`json:${space}`, () => {
      // Refetch initial items on change
      fetchPage(true);
    });

    return unsubscribe;
  }, [space, resolvedDb, fetchPage]);

  const handleRefresh = useCallback(() => {
    setIsRefreshing(true);
    fetchPage(true);
  }, [fetchPage]);

  const handleEndReached = useCallback(() => {
    if (!hasMore || isLoadingRef.current) return;
    setIsLoadingMore(true);
    fetchPage(false);
  }, [hasMore, fetchPage]);

  if (isLoading && loadingComponent) {
    return <>{loadingComponent}</>;
  }

  if (error && errorComponent) {
    return <>{errorComponent(error)}</>;
  }

  const defaultKeyExtractor = (item: JsonEntry<T>, index: number): string => {
    if (keyExtractor) return keyExtractor(item, index);
    return item.id || String(index);
  };

  const footerComponent = isLoadingMore ? (
    <View style={{ paddingVertical: 16, alignItems: 'center' }}>
      <ActivityIndicator size="small" />
    </View>
  ) : (
    flatListProps.ListFooterComponent
  );

  return (
    <FlatList<JsonEntry<T>>
      data={items}
      renderItem={renderItem}
      keyExtractor={defaultKeyExtractor}
      onEndReached={handleEndReached}
      onEndReachedThreshold={onEndReachedThreshold}
      refreshControl={
        <RefreshControl refreshing={isRefreshing} onRefresh={handleRefresh} />
      }
      ListEmptyComponent={
        emptyComponent ? () => <>{emptyComponent}</> : flatListProps.ListEmptyComponent
      }
      ListFooterComponent={footerComponent}
      {...flatListProps}
    />
  );
}
