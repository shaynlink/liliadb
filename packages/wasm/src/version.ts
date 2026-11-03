import { LiliaError, type JsonValue } from "./types.js";

const MAX_U64 = (1n << 64n) - 1n;

export function randomUuid(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g, c => {
    const r = (Math.random() * 16) | 0;
    const v = c === "x" ? r : (r & 0x3) | 0x8;
    return v.toString(16);
  });
}

export function checkedExpiry(value: number | undefined): number | undefined {
  if (value !== undefined && !Number.isSafeInteger(value)) {
    throw new LiliaError({
      code: "INVALID_INPUT",
      message: "expiresAtMs must be a safe integer",
      retryable: false,
      request_id: randomUuid(),
    });
  }
  return value;
}

export function checkedVersion(value: bigint | undefined): bigint | undefined {
  if (value === undefined) return undefined;
  if (typeof value !== "bigint" || value < 0n || value > MAX_U64) {
    throw new LiliaError({
      code: "INVALID_INPUT",
      message: "ifVersion must be a bigint in 0..=18446744073709551615",
      retryable: false,
      request_id: randomUuid(),
    });
  }
  return value;
}

export function readVersion(value: unknown): bigint {
  if (typeof value === "number" && (!Number.isSafeInteger(value) || value < 0)) {
    throw new Error("unsafe wire version");
  }
  if (
    typeof value !== "number" &&
    typeof value !== "bigint" &&
    !(typeof value === "string" && /^(0|[1-9][0-9]*)$/.test(value))
  ) {
    throw new Error("invalid wire version");
  }
  const version = BigInt(value as string | number | bigint);
  if (version < 0n || version > MAX_U64) {
    throw new Error("wire version exceeds u64");
  }
  return version;
}

export function jsonNumbers(value: unknown): JsonValue {
  if (typeof value === "bigint") return Number(value);
  if (Array.isArray(value)) return value.map(jsonNumbers);
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value).map(([key, item]) => [key, jsonNumbers(item)])
    );
  }
  return value as JsonValue;
}
