import React from 'react';
import { useDocument } from '../../hooks.js';
import type { LiliaDatabase } from '../../database.js';

export interface LiliaDocumentViewProps<T = unknown> {
  space: string;
  id: string;
  defaultValue?: T;
  db?: LiliaDatabase | null;
  loadingComponent?: React.ReactNode;
  errorComponent?: (error: Error) => React.ReactNode;
  children: (props: {
    doc: T | null;
    isLoading: boolean;
    error: Error | null;
    update: (patch: Partial<T> | T | ((prev: T | null) => T)) => Promise<void>;
    remove: () => Promise<void>;
    reload: () => Promise<void>;
  }) => React.ReactNode;
}

/**
 * Idiomatic React Native render-prop component observing a single JSON document.
 * Isolates re-renders strictly to this component's subtree.
 */
export function LiliaDocumentView<T = Record<string, unknown>>({
  space,
  id,
  defaultValue,
  db,
  loadingComponent,
  errorComponent,
  children,
}: LiliaDocumentViewProps<T>): React.ReactElement | null {
  const meta = useDocument<T>(space, id, { defaultValue, db });

  if (meta.isLoading && loadingComponent) {
    return <>{loadingComponent}</>;
  }

  if (meta.error && errorComponent) {
    return <>{errorComponent(meta.error)}</>;
  }

  const rendered = children({
    doc: meta.data,
    isLoading: meta.isLoading,
    error: meta.error,
    update: meta.update,
    remove: meta.remove,
    reload: meta.reload,
  });

  return rendered ? <>{rendered}</> : null;
}
