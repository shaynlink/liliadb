import React, { useState, useEffect } from 'react';
import { LiliaContext } from './context.js';
import { LiliaDatabase } from './database.js';
import type { LiliaProviderProps } from './types.js';

export function LiliaProvider(props: LiliaProviderProps): React.ReactElement {
  // Mode 1: Externally provided database
  if ('database' in props && props.database) {
    return (
      <LiliaContext.Provider value={props.database}>
        {props.children}
      </LiliaContext.Provider>
    );
  }

  // Mode 2: Auto-managed options
  const { options, fallback, errorFallback, children } = props;
  const [db, setDb] = useState<LiliaDatabase | null>(null);
  const [error, setError] = useState<Error | null>(null);
  const [isLoading, setIsLoading] = useState(true);

  useEffect(() => {
    let active = true;
    let databaseInstance: LiliaDatabase | null = null;

    setIsLoading(true);
    setError(null);

    LiliaDatabase.open(options)
      .then((database) => {
        if (active) {
          databaseInstance = database;
          setDb(database);
          setIsLoading(false);
        } else {
          database.close().catch(() => {});
        }
      })
      .catch((err) => {
        if (active) {
          setError(err instanceof Error ? err : new Error(String(err)));
          setIsLoading(false);
        }
      });

    return () => {
      active = false;
      if (databaseInstance) {
        databaseInstance.close().catch(() => {});
      }
    };
  }, [options.path, options.profile, options.useSecureStorage, options.passphrase]);

  if (error && errorFallback) {
    return <>{errorFallback(error)}</>;
  }

  if (isLoading) {
    return <>{fallback ?? null}</>;
  }

  return (
    <LiliaContext.Provider value={db}>
      {children}
    </LiliaContext.Provider>
  );
}
