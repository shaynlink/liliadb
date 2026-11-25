import React, { createContext, useContext } from "react";
import type { LiliaDatabase } from "./database.js";

export const LiliaContext = createContext<LiliaDatabase | null>(null);

/**
 * The database `explicitDb`, else the one of the nearest `<LiliaProvider>`.
 *
 * @throws Error outside a `<LiliaProvider>` that has opened its database, without `explicitDb`.
 */
export function useDatabase(explicitDb?: LiliaDatabase | null): LiliaDatabase {
  const provided = useContext(LiliaContext);
  const db = explicitDb ?? provided;
  if (!db) {
    throw new Error("[LiliaDB] no database: render this inside a <LiliaProvider> or pass a database");
  }
  return db;
}

/** `useDatabase`. */
export const useLilia = useDatabase;

/** The database of the nearest `<LiliaProvider>`, for a render function. */
export function LiliaConsumer({ children }: { children: (db: LiliaDatabase) => React.ReactNode }): React.ReactElement {
  const db = useDatabase();
  return <>{children(db)}</>;
}
