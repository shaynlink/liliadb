#pragma once

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct LiliaHandle LiliaHandle;

// Memory management
void lilia_c_free_string(char* ptr);
void lilia_c_free_bytes(uint8_t* ptr, size_t len);
void lilia_c_free_handle(LiliaHandle* handle);

// Lifecycle & Database creation
LiliaHandle* lilia_c_open(
    const char* path,
    int profile_code,
    const uint8_t* key_bytes,
    size_t key_len,
    const char* passphrase,
    char** out_err
);
int lilia_c_close(LiliaHandle* handle, uint64_t timeout_ms);
int lilia_c_prepare_for_background(LiliaHandle* handle);
int lilia_c_checkpoint(LiliaHandle* handle);
int lilia_c_vacuum(LiliaHandle* handle);
int lilia_c_get_file_size(LiliaHandle* handle, uint64_t* out_main, uint64_t* out_wal, uint64_t* out_shm);
int lilia_c_backup(LiliaHandle* handle, const char* dest);

// Key / Value
uint8_t* lilia_c_kv_get(
    LiliaHandle* handle,
    const char* ns,
    const uint8_t* key,
    size_t key_len,
    size_t* out_val_len,
    uint64_t* out_version,
    int64_t* out_expires
);
int lilia_c_kv_set(
    LiliaHandle* handle,
    const char* ns,
    const uint8_t* key,
    size_t key_len,
    const uint8_t* val,
    size_t val_len,
    int64_t expires_at_ms,
    uint64_t* out_version
);
int lilia_c_kv_delete(
    LiliaHandle* handle,
    const char* ns,
    const uint8_t* key,
    size_t key_len,
    int* out_deleted
);

// JSON Document
char* lilia_c_json_get(LiliaHandle* handle, const char* space, const char* id, uint64_t* out_version);
int lilia_c_json_put(LiliaHandle* handle, const char* space, const char* id, const char* val_json, uint64_t* out_version);
int lilia_c_json_delete(LiliaHandle* handle, const char* space, const char* id, int* out_deleted);
int lilia_c_json_find(LiliaHandle* handle, const char* space, const char* query_json, const char* after_id, uint32_t limit, char** out_json);

// Graph
int lilia_c_graph_set_node(LiliaHandle* handle, const char* graph, const char* id, const char* kind, const char* metadata_json);
int lilia_c_graph_get_node(LiliaHandle* handle, const char* graph, const char* id, char** out_json);
int lilia_c_graph_delete_node(LiliaHandle* handle, const char* graph, const char* id, int* out_deleted);
int lilia_c_graph_add_edge(LiliaHandle* handle, const char* graph, const char* source, const char* target, const char* kind);
int lilia_c_graph_remove_edge(LiliaHandle* handle, const char* graph, const char* source, const char* target, const char* kind, int* out_deleted);
int lilia_c_graph_neighbors(LiliaHandle* handle, const char* graph, const char* node, int direction, const char* kind, char** out_json);
int lilia_c_graph_reachability(LiliaHandle* handle, const char* graph, const char* start_nodes_json, int direction, char** out_json);

// FTS5
int lilia_c_fts_index(LiliaHandle* handle, const char* collection, const char* doc_id, const char* content);
int lilia_c_fts_delete(LiliaHandle* handle, const char* collection, const char* doc_id, int* out_deleted);
int lilia_c_fts_search(LiliaHandle* handle, const char* collection, const char* query, uint32_t limit, char** out_json);

// SQL
int lilia_c_sql_query(LiliaHandle* handle, const char* sql, const char* params_json, char** out_cols_json, char** out_rows_json, uint64_t* out_affected);

#ifdef __cplusplus
}
#endif
