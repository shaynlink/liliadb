import React, { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { useDocument } from '../../hooks.js';
import type { LiliaDatabase } from '../../database.js';

export interface FormHelpers<T> {
  values: T;
  setFieldValue: <K extends keyof T>(field: K, value: T[K]) => void;
  setValues: (values: T | ((prev: T) => T)) => void;
  isDirty: boolean;
  isSubmitting: boolean;
  isLoading: boolean;
  error: Error | null;
  submit: () => Promise<void>;
  reset: () => void;
}

export interface LiliaFormProps<T extends Record<string, any>> {
  space: string;
  id: string;
  initialValues?: T;
  db?: LiliaDatabase | null;
  onSubmit?: (values: T) => Promise<void> | void;
  onSuccess?: (values: T) => void;
  onError?: (error: Error) => void;
  children: (helpers: FormHelpers<T>) => React.ReactNode;
}

/**
 * Idiomatic declarative form container bound directly to a LiliaDB JSON document.
 * Tracks field values, detects dirty state, and performs atomic commits.
 */
export function LiliaForm<T extends Record<string, any>>({
  space,
  id,
  initialValues,
  db,
  onSubmit,
  onSuccess,
  onError,
  children,
}: LiliaFormProps<T>): React.ReactElement | null {
  const { data: remoteDoc, isLoading, error: docError, update } = useDocument<T>(space, id, {
    defaultValue: initialValues,
    db,
  });

  const baseValues = useMemo<T>(() => {
    return remoteDoc ?? initialValues ?? ({} as T);
  }, [remoteDoc, initialValues]);

  const [values, setValuesState] = useState<T>(baseValues);
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [error, setError] = useState<Error | null>(docError);
  const baseValuesRef = useRef<T>(baseValues);

  useEffect(() => {
    baseValuesRef.current = baseValues;
    setValuesState(baseValues);
  }, [baseValues]);

  const isDirty = useMemo(() => {
    try {
      return JSON.stringify(values) !== JSON.stringify(baseValuesRef.current);
    } catch {
      return values !== baseValuesRef.current;
    }
  }, [values]);

  const setFieldValue = useCallback(<K extends keyof T>(field: K, val: T[K]) => {
    setValuesState((prev) => ({
      ...prev,
      [field]: val,
    }));
  }, []);

  const setValues = useCallback((newValues: T | ((prev: T) => T)) => {
    setValuesState(newValues);
  }, []);

  const reset = useCallback(() => {
    setValuesState(baseValuesRef.current);
  }, []);

  const submit = useCallback(async () => {
    setIsSubmitting(true);
    setError(null);
    try {
      if (onSubmit) {
        await onSubmit(values);
      }
      await update(values);
      baseValuesRef.current = values;
      onSuccess?.(values);
    } catch (err) {
      const e = err instanceof Error ? err : new Error(String(err));
      setError(e);
      onError?.(e);
    } finally {
      setIsSubmitting(false);
    }
  }, [values, onSubmit, update, onSuccess, onError]);

  const helpers: FormHelpers<T> = {
    values,
    setFieldValue,
    setValues,
    isDirty,
    isSubmitting,
    isLoading,
    error,
    submit,
    reset,
  };

  const rendered = children(helpers);
  return rendered ? <>{rendered}</> : null;
}
