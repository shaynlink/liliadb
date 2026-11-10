import React, { useCallback } from 'react';
import { Switch, type SwitchProps } from 'react-native';
import { useKvState } from '../../hooks.js';
import type { LiliaDatabase } from '../../database.js';

export interface LiliaKvSwitchProps extends Omit<SwitchProps, 'value' | 'onValueChange'> {
  namespace: string;
  storageKey: string;
  defaultValue?: boolean;
  db?: LiliaDatabase | null;
  onValueChange?: (value: boolean) => void;
}

/**
 * Idiomatic zero-boilerplate Switch component bound directly to LiliaDB Key-Value storage.
 * Reads synchronously on initial render (Zero FOUC) and toggles immediately.
 */
export function LiliaKvSwitch({
  namespace,
  storageKey,
  defaultValue = false,
  db,
  onValueChange,
  ...switchProps
}: LiliaKvSwitchProps): React.ReactElement {
  const [value, setValue] = useKvState<boolean>(namespace, storageKey, defaultValue, { db });

  const handleValueChange = useCallback(
    (newValue: boolean) => {
      void setValue(newValue);
      onValueChange?.(newValue);
    },
    [setValue, onValueChange]
  );

  return (
    <Switch
      value={Boolean(value)}
      onValueChange={handleValueChange}
      {...switchProps}
    />
  );
}
