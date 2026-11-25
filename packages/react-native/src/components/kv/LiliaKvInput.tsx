import React, { useState, useEffect, useRef, useCallback } from "react";
import { TextInput, type TextInputProps } from "react-native";
import { useKv } from "../../hooks.js";
import type { LiliaDatabase } from "../../database.js";

export interface LiliaKvInputProps extends Omit<TextInputProps, "value"> {
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
  defaultValue = "",
  debounceMs = 300,
  db,
  onChangeText,
  onBlur,
  ...inputProps
}: LiliaKvInputProps): React.ReactElement {
  const [storedValue, setStoredValue] = useKv<string>(namespace, storageKey, {
    defaultValue,
    db,
  });

  // What the user typed and is not stored yet; the stored value (which other writers may change)
  // shows when there is none.
  const [draft, setDraft] = useState<string | null>(null);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    return () => {
      if (timerRef.current) {
        clearTimeout(timerRef.current);
      }
    };
  }, []);

  /** Stores `text`, then shows the stored value again unless the user typed since. */
  const store = useCallback(
    async (text: string) => {
      await setStoredValue(text);
      setDraft((current) => (current === text ? null : current));
    },
    [setStoredValue],
  );

  const handleChangeText = useCallback(
    (text: string) => {
      setDraft(text);
      onChangeText?.(text);

      if (timerRef.current) {
        clearTimeout(timerRef.current);
      }

      timerRef.current = setTimeout(() => {
        timerRef.current = null;
        void store(text);
      }, debounceMs);
    },
    [debounceMs, onChangeText, store],
  );

  const handleBlur = useCallback<NonNullable<TextInputProps["onBlur"]>>(
    (event) => {
      if (timerRef.current) {
        clearTimeout(timerRef.current);
        timerRef.current = null;
      }
      if (draft !== null) {
        void store(draft);
      }
      onBlur?.(event);
    },
    [draft, store, onBlur],
  );

  return (
    <TextInput
      {...inputProps}
      value={draft ?? storedValue ?? defaultValue}
      onChangeText={handleChangeText}
      onBlur={handleBlur}
    />
  );
}
