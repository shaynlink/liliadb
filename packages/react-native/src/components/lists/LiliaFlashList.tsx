import React, { useMemo } from "react";
import { FlatList, RefreshControl, type ListRenderItem } from "react-native";
import type { JsonValue } from "@liliadb/client";
import { useSqlQuery } from "../../hooks.js";
import type { LiliaDatabase } from "../../database.js";

/** `@shopify/flash-list`'s list, when the app installed it; `FlatList` is used otherwise. */
const FlashListComponent: React.ComponentType<any> | null = (() => {
  try {
    return (
      (require("@shopify/flash-list") as { FlashList?: React.ComponentType<any> }).FlashList ?? null
    );
  } catch {
    return null;
  }
})();

export interface LiliaFlashListProps<T> {
  sql?: string;
  sqlParams?: JsonValue[];
  tables?: string[];
  data?: T[];
  estimatedItemSize: number;
  renderItem: ListRenderItem<T>;
  keyExtractor?: (item: T, index: number) => string;
  loadingComponent?: React.ReactNode;
  emptyComponent?: React.ReactNode;
  errorComponent?: (error: Error) => React.ReactNode;
  db?: LiliaDatabase | null;
  [key: string]: any;
}

/**
 * High-performance list component integrating with `@shopify/flash-list`.
 * Falls back cleanly to `FlatList` if `@shopify/flash-list` is not installed.
 */
export function LiliaFlashList<T = Record<string, unknown>>({
  sql,
  sqlParams,
  tables,
  data: staticData,
  estimatedItemSize,
  renderItem,
  keyExtractor,
  loadingComponent,
  emptyComponent,
  errorComponent,
  db,
  ...restProps
}: LiliaFlashListProps<T>): React.ReactElement {
  const queryResult = useSqlQuery<T>(sql ?? "", sqlParams, {
    tables,
    enabled: Boolean(sql),
    db,
  });

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
    if (item && typeof item === "object") {
      const anyObj = item as Record<string, unknown>;
      if ("id" in anyObj && anyObj.id != null) return String(anyObj.id);
      if ("rowid" in anyObj && anyObj.rowid != null) return String(anyObj.rowid);
      if ("key" in anyObj && anyObj.key != null) return String(anyObj.key);
    }
    return String(index);
  };

  const refreshControl = sql ? (
    <RefreshControl refreshing={queryResult.isRefetching} onRefresh={queryResult.refetch} />
  ) : (
    restProps.refreshControl
  );

  if (FlashListComponent) {
    return (
      <FlashListComponent
        {...restProps}
        data={items}
        renderItem={renderItem}
        estimatedItemSize={estimatedItemSize}
        keyExtractor={defaultKeyExtractor}
        ListEmptyComponent={emptyComponent ? <>{emptyComponent}</> : restProps.ListEmptyComponent}
        refreshControl={refreshControl}
      />
    );
  }

  return (
    <FlatList<T>
      {...restProps}
      data={items}
      renderItem={renderItem}
      keyExtractor={defaultKeyExtractor}
      ListEmptyComponent={emptyComponent ? <>{emptyComponent}</> : restProps.ListEmptyComponent}
      refreshControl={refreshControl}
    />
  );
}
