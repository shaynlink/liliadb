// @liliadb/client: the LiliaDB calls, written once for every JavaScript runtime. A runtime
// package (`@liliadb/node`, `@liliadb/react-native`) opens a transport to the engine and builds
// its database on `Database`.

export * from "./types.js";
export { geo, quantization, timeseries } from "./math.js";
export type { Api, Mode, Out } from "./api.js";
export { Database, abortable, lifecycleError, type Transport } from "./database.js";
export { getUuid } from "./version.js";
export {
  bytesOf,
  encodeWire,
  expect,
  frameJsonLength,
  readFrame,
  unwrap,
  wireError,
  type Operation,
  type ResponseOf,
  type ResponseType,
  type ResponseValue,
  type WireBatchOperation,
  type WireError,
  type WireResult,
} from "./wire.js";
