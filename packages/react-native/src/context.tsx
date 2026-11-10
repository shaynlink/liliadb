import React, { createContext, useContext } from 'react';
import type { LiliaDatabase } from './database.js';

export const LiliaContext = createContext<LiliaDatabase | null>(null);

/**
 * Access the active LiliaDatabase from the nearest `<LiliaProvider>`,
 * or fall back to an explicitly supplied database instance.
 *
 * @throws Error if called outside a `<LiliaProvider>` and no explicit db is provided.
 */
export function useDatabase(explicitDb?: LiliaDatabase | null): LiliaDatabase {
  const contextDb = useContext(LiliaContext);
  const db = explicitDb ?? contextDb;

  if (!db) {
    throw new Error(
      `[LiliaDB] No LiliaDatabase instance found. ` +
      `Wrap your root component tree in <LiliaProvider> or pass an explicit database instance.`
    );
  }

  return db;
}

/**
 * Convenient shorthand for `useDatabase()`.
 */
export const useLilia = useDatabase;

/**
 * Render prop component to consume the database instance without hooks.
 */
export function LiliaConsumer({
  children,
}: {
  children: (db: LiliaDatabase) => React.ReactNode;
}): React.ReactElement {
  const db = useDatabase();
  return <>{children(db)}</>;
}
