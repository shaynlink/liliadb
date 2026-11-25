import React, { useState, useCallback, useMemo } from "react";
import { useDocument } from "../../hooks.js";
import type { LiliaDatabase } from "../../database.js";

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
  const {
    data: remoteDoc,
    isLoading,
    error: docError,
    update,
  } = useDocument<T>(space, id, {
    defaultValue: initialValues,
    db,
  });

  const baseValues = useMemo<T>(() => {
    return remoteDoc ?? initialValues ?? ({} as T);
  }, [remoteDoc, initialValues]);

  // The user's values while they differ from the document; the document (which other writers
  // may change) shows when there are none.
  const [edits, setEdits] = useState<T | null>(null);
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<Error | null>(null);
  const values = edits ?? baseValues;

  const isDirty = useMemo(() => {
    if (edits === null) return false;
    try {
      return JSON.stringify(edits) !== JSON.stringify(baseValues);
    } catch {
      return edits !== baseValues;
    }
  }, [edits, baseValues]);

  const setFieldValue = useCallback(
    <K extends keyof T>(field: K, val: T[K]) => {
      setEdits((prev) => ({
        ...(prev ?? baseValues),
        [field]: val,
      }));
    },
    [baseValues],
  );

  const setValues = useCallback(
    (newValues: T | ((prev: T) => T)) => {
      setEdits((prev) =>
        typeof newValues === "function" ? newValues(prev ?? baseValues) : newValues,
      );
    },
    [baseValues],
  );

  const reset = useCallback(() => {
    setEdits(null);
  }, []);

  const submit = useCallback(async () => {
    setIsSubmitting(true);
    setSubmitError(null);
    try {
      if (onSubmit) {
        await onSubmit(values);
      }
      await update(values);
      // The document now holds these values; edits made during the commit stay.
      setEdits((current) => (current === values ? null : current));
      onSuccess?.(values);
    } catch (err) {
      const e = err instanceof Error ? err : new Error(String(err));
      setSubmitError(e);
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
    error: submitError ?? docError,
    submit,
    reset,
  };

  const rendered = children(helpers);
  return rendered ? <>{rendered}</> : null;
}
