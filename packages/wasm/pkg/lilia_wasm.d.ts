/* tslint:disable */
/* eslint-disable */

export class WasmDatabase {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    batch(operations_json: string): string;
    close(timeout_ms?: number | null): void;
    fts_search(collection: string, query: string, limit?: number | null): any;
    graph_neighbors(graph: string, node: string, direction?: string | null, kind?: string | null): any;
    graph_reachability(graph: string, start_nodes_val: any, direction?: string | null): any;
    integrity_check(): boolean;
    json_create_index(space: string, name: string, path: string): void;
    json_drop_index(space: string, name: string): boolean;
    json_find(space: string, query_json: string, after: string | null | undefined, limit: number): string;
    json_get(space: string, id: string): string | undefined;
    json_list_indexes(space: string): string;
    json_scan(space: string, after: string | null | undefined, limit: number): string;
    json_scan_by_index(space: string, index_name: string, value: string, after: string | null | undefined, limit: number): string;
    kv_get(namespace: string, key: Uint8Array): any;
    kv_scan(namespace: string, after: Uint8Array | null | undefined, limit: number): any;
    static open(options_val: any): Promise<WasmDatabase>;
    sql_query(sql: string, params_json?: string | null): string;
    vector_get(collection: string, id: string): Uint8Array | undefined;
    vector_search(collection: string, query_vector: Uint8Array, dim: number, element_type?: string | null, metric?: string | null, limit?: number | null): any;
    hybrid_search(collection: string, fts_query?: string | null, vector_query?: Uint8Array | null, dim?: number | null, element_type?: string | null, metric?: string | null, rrf_k?: number | null, fts_weight?: number | null, vector_weight?: number | null, limit?: number | null): any;
}

export function init_opfs_vfs(directory?: string | null): Promise<void>;

export function is_opfs_supported(): boolean;

export function start(): void;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_wasmdatabase_free: (a: number, b: number) => void;
    readonly init_opfs_vfs: (a: number, b: number) => number;
    readonly is_opfs_supported: () => number;
    readonly start: () => void;
    readonly wasmdatabase_batch: (a: number, b: number, c: number, d: number) => void;
    readonly wasmdatabase_close: (a: number, b: number, c: number) => void;
    readonly wasmdatabase_fts_search: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => void;
    readonly wasmdatabase_graph_neighbors: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number) => void;
    readonly wasmdatabase_graph_reachability: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => void;
    readonly wasmdatabase_integrity_check: (a: number, b: number) => void;
    readonly wasmdatabase_json_create_index: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => void;
    readonly wasmdatabase_json_drop_index: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly wasmdatabase_json_find: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number) => void;
    readonly wasmdatabase_json_get: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly wasmdatabase_json_list_indexes: (a: number, b: number, c: number, d: number) => void;
    readonly wasmdatabase_json_scan: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => void;
    readonly wasmdatabase_json_scan_by_index: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number) => void;
    readonly wasmdatabase_kv_get: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly wasmdatabase_kv_scan: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => void;
    readonly wasmdatabase_open: (a: number) => number;
    readonly wasmdatabase_sql_query: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly rust_sqlite_wasm_abort: () => void;
    readonly rust_sqlite_wasm_assert_fail: (a: number, b: number, c: number, d: number) => void;
    readonly rust_sqlite_wasm_calloc: (a: number, b: number) => number;
    readonly rust_sqlite_wasm_free: (a: number) => void;
    readonly rust_sqlite_wasm_getentropy: (a: number, b: number) => number;
    readonly rust_sqlite_wasm_localtime: (a: number) => number;
    readonly rust_sqlite_wasm_malloc: (a: number) => number;
    readonly rust_sqlite_wasm_realloc: (a: number, b: number) => number;
    readonly sqlite3_os_end: () => number;
    readonly sqlite3_os_init: () => number;
    readonly __wasm_bindgen_func_elem_2890: (a: number, b: number, c: number, d: number) => void;
    readonly __wasm_bindgen_func_elem_2916: (a: number, b: number, c: number, d: number) => void;
    readonly __wbindgen_export: (a: number, b: number) => number;
    readonly __wbindgen_export2: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_export3: (a: number) => void;
    readonly __wbindgen_export4: (a: number, b: number) => void;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
    readonly __wbindgen_export5: (a: number, b: number, c: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
