// The fixtures name versions instead of stating them (a version is the engine's sequence number
// of a write): `"$kv_a1"` binds the version a step returns, later steps compare with it or send
// it as an `if_version`, and two names never bind the same version. The Rust runner
// (crates/lilia-protocol/tests/conformance.rs) applies the same rules.

import assert from "node:assert/strict";

export class Versions {
  #bound = new Map();

  /** `value` with its named versions replaced by their values. */
  substitute(value) {
    if (typeof value === "string" && value.startsWith("$")) {
      assert(this.#bound.has(value), `${value} is not bound yet`);
      return this.#bound.get(value);
    }
    if (Array.isArray(value)) return value.map((item) => this.substitute(item));
    if (value !== null && typeof value === "object") {
      return Object.fromEntries(
        Object.entries(value).map(([key, item]) => [key, this.substitute(item)]),
      );
    }
    return value;
  }

  /** Whether `actual` matches `expected`, binding the names `expected` uses for the first time. */
  matches(actual, expected) {
    if (typeof expected === "string" && expected.startsWith("$") && typeof actual === "string") {
      if (this.#bound.has(expected)) return this.#bound.get(expected) === actual;
      assert(
        ![...this.#bound.values()].includes(actual),
        `${expected} binds ${actual}, which another name already holds`,
      );
      this.#bound.set(expected, actual);
      return true;
    }
    if (Array.isArray(expected)) {
      return (
        Array.isArray(actual) &&
        actual.length === expected.length &&
        expected.every((item, i) => this.matches(actual[i], item))
      );
    }
    if (expected !== null && typeof expected === "object") {
      if (actual === null || typeof actual !== "object" || Array.isArray(actual)) return false;
      const keys = Object.keys(expected);
      return (
        Object.keys(actual).length === keys.length &&
        keys.every((key) => key in actual && this.matches(actual[key], expected[key]))
      );
    }
    return Object.is(actual, expected) || actual === expected;
  }
}
