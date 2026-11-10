import React, { useState, useEffect, useRef, useCallback } from 'react';
import { TextInput, type TextInputProps } from 'react-native';
import { useKv } from '../../hooks.js';
import type { LiliaDatabase } from '../../database.js';

export interface LiliaKvInputProps extends Omit<TextInputProps, 'value'> {
  namespace: string;
  storageKey: string;
  defaultValue?: string;
  debounceMs?: number;
  db?: LiliaDatabase | null;
}

/**
 * Idiomatic TextInput component with automatic debounced persistence into LiliaDB Key-Value.
 * Perfect for persistent draft forms, search inputs, and temporary state.
 */
export function LiliaKvInput({
  namespace,
  storageKey,
  defaultValue = '',
  debounceMs = 300,
  db,
  onChangeText,
  ...inputProps
}: LiliaKvInputProps): React.ReactElement {
  const [storedValue, setStoredValue] = useKv<string>(namespace, storageKey, {
    defaultValue,
    db,
  });

  const [localText, setLocalText] = useState<string>(storedValue ?? defaultValue);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const isEditingRef = useRef(false);

  // Sync external changes if not actively typing
  useEffect(() => {
    if (!isEditingRef.current && storedValue != null && storedValue !== localText) {
      setLocalText(storedValue);
    }
  }, [storedValue]);

  useEffect(() => {
    return () => {
      if (timerRef.current) {
        clearTimeout(timerRef.current);
      }
    };
  }, []);

  const handleChangeText = useCallback(
    (text: string) => {
      isEditingRef.current = true;
      setLocalText(text);
      onChangeText?.(text);

      if (timerRef.current) {
        clearTimeout(timerRef.current);
      }

      timerRef.current = setTimeout(() => {
        isEditingRef.current = false;
        void setStoredValue(text);
      }, debounceMs);
    },
    [debounceMs, onChangeText, setStoredValue]
  );

  const handleBlur = useCallback(
    (e: any) => {
      if (timerRef.current) {
        clearTimeout(timerRef.current);
      }
      isEditingRef.current = false;
      void setStoredValue(localText);
      inputProps.onBlur?.(e);
    },
    [localText, setStoredValue, inputProps.onBlur]
  );

  return (
    <TextInput
      value={localText}
      onChangeText={handleChangeText}
      onBlur={handleBlur}
      {...inputProps}
    />
  );
}
