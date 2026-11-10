import React from 'react';

export interface LiliaSuspenseProps {
  fallback: React.ReactNode;
  isReady?: boolean;
  isLoading?: boolean;
  children: React.ReactNode;
}

/**
 * Idiomatic Zero-FOUC suspension wrapper for React Native.
 * Renders fallback skeleton while database state is hydrating or queries are executing.
 */
export function LiliaSuspense({
  fallback,
  isReady = true,
  isLoading = false,
  children,
}: LiliaSuspenseProps): React.ReactElement {
  if (isLoading || !isReady) {
    return <>{fallback}</>;
  }

  return <>{children}</>;
}
