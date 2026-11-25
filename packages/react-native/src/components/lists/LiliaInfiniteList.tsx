import React, { useState, useEffect, useCallback, useRef } from 'react';
import {
  FlatList,
  RefreshControl,
  ActivityIndicator,
  View,
  type FlatListProps,
  type ListRenderItem,
} from 'react-native';
import type { JsonEntry, JsonValue } from '@liliadb/client';
import { topics } from '../../changes.js';
import { useDatabase } from '../../context.js';
import type { LiliaDatabase } from '../../database.js';

export interface LiliaInfiniteListProps<T = JsonValue>
  extends Omit<FlatListProps<JsonEntry<T>>, 'data' | 'renderItem'> {
  space: string;
  pageSize?: number;
  /** The documents shown, as `json.find` selects them; all of them by default. */
  filter?: Record<string, JsonValue>;
  renderItem: ListRenderItem<JsonEntry<T>>;
  loadingComponent?: React.ReactNode;
  emptyComponent?: React.ReactNode;
  errorComponent?: (error: Error) => React.ReactNode;
  db?: LiliaDatabase | null;
}

/**
 * Idiomatic React Native infinite scrolling list of the documents of a JSON space, a page at
 * a time in id order. A write to the space reads the pages shown again.
 */
export function LiliaInfiniteList<T = JsonValue>({
  space,
  pageSize = 20,
  filter,
  renderItem,
  keyExtractor,
  loadingComponent,
  emptyComponent,
  errorComponent,
  db: explicitDb,
  onEndReachedThreshold = 0.5,
  ...flatListProps
}: LiliaInfiniteListProps<T>): React.ReactElement {
  const db = useDatabase(explicitDb);
  const [items, setItems] = useState<JsonEntry<T>[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [isLoadingMore, setIsLoadingMore] = useState(false);
  const [hasMore, setHasMore] = useState(true);
  const [error, setError] = useState<Error | null>(null);

  const filterKey = JSON.stringify(filter ?? {});
  const itemsRef = useRef(items);
  itemsRef.current = items;
  const loading = useRef(false);
  /** Counts reads: a read that a newer one replaced shows nothing. */
  const sequence = useRef(0);

  /** Reads `limit` documents after `after`, appended to the list or replacing it. */
  const read = useCallback(
    async (after: string | undefined, limit: number, append: boolean) => {
      const run = ++sequence.current;
      loading.current = true;
      try {
        const query = JSON.parse(filterKey) as Record<string, JsonValue>;
        const page = await db.json.find<T>(space, query, { after, limit });
        if (run !== sequence.current) return;
        setItems((previous) => (append ? [...previous, ...page] : page));
        setHasMore(page.length >= limit);
        setError(null);
      } catch (failure) {
        if (run === sequence.current) setError(failure instanceof Error ? failure : new Error(String(failure)));
      } finally {
        if (run === sequence.current) {
          loading.current = false;
          setIsLoading(false);
          setIsRefreshing(false);
          setIsLoadingMore(false);
        }
      }
    },
    [db, space, filterKey]
  );

  useEffect(() => {
    setIsLoading(true);
    void read(undefined, pageSize, false);
    // A write reads again as many documents as the list shows.
    return db.subscribe(topics.json(space), () => {
      void read(undefined, Math.max(pageSize, itemsRef.current.length), false);
    });
  }, [db, space, pageSize, read]);

  const handleRefresh = useCallback(() => {
    setIsRefreshing(true);
    void read(undefined, pageSize, false);
  }, [read, pageSize]);

  const handleEndReached = useCallback(() => {
    if (!hasMore || loading.current) return;
    setIsLoadingMore(true);
    void read(itemsRef.current[itemsRef.current.length - 1]?.id, pageSize, true);
  }, [hasMore, read, pageSize]);

  if (isLoading && loadingComponent) {
    return <>{loadingComponent}</>;
  }

  if (error && errorComponent) {
    return <>{errorComponent(error)}</>;
  }

  const defaultKeyExtractor = (item: JsonEntry<T>, index: number): string => {
    if (keyExtractor) return keyExtractor(item, index);
    return item.id;
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
