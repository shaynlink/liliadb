import React from "react";
import { LiliaContext } from "./context.js";
import { useLiliaDatabase } from "./hooks.js";
import type { LiliaDatabase } from "./database.js";
import type { LiliaProviderProps, OpenDatabaseOptions } from "./types.js";

/** Opens the database of `options` while it is mounted. */
function OpenedProvider(props: {
  options: OpenDatabaseOptions;
  children: React.ReactNode;
  fallback?: React.ReactNode;
  errorFallback?: (error: Error) => React.ReactNode;
}): React.ReactElement {
  const { db, error } = useLiliaDatabase(props.options);
  if (error) {
    if (props.errorFallback) return <>{props.errorFallback(error)}</>;
    throw error;
  }
  if (!db) return <>{props.fallback ?? null}</>;
  return <LiliaContext.Provider value={db}>{props.children}</LiliaContext.Provider>;
}

/**
 * Gives its children a database: `database`, or the one it opens from `options` (showing
 * `fallback` meanwhile) and closes when it unmounts.
 */
export function LiliaProvider(props: LiliaProviderProps): React.ReactElement {
  if (props.database) {
    return (
      <LiliaContext.Provider value={props.database as LiliaDatabase}>
        {props.children}
      </LiliaContext.Provider>
    );
  }
  return (
    <OpenedProvider
      options={props.options}
      fallback={props.fallback}
      errorFallback={props.errorFallback}
    >
      {props.children}
    </OpenedProvider>
  );
}
