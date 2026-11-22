// The C ABI of liblilia_ffi (crates/lilia-ffi/src/c_abi.rs): open a database, run operations,
// close it. An operation goes in as MessagePack (encoding 0) or JSON (1); its result comes out
// as a JS frame: [u32 JSON length][JSON][u32 segment count][(u32 offset, u32 length) per
// segment][segment bytes], all little-endian, the JSON being {"Ok": ...} or {"Err": ...}.

#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct LiliaHandle LiliaHandle;

/// Opens the database `options_json` describes ({"path": ..., "durability": ..., "profile":
/// ..., "ledger": ..., "history_retention_ms": ..., "undo_depth": ...}, all but path
/// optional). Null on failure, with the error's JSON through `out_err`.
LiliaHandle *lilia_c_open(const char *options_json, char **out_err);

/// Runs the operation `request` holds and returns the JS frame of its result, with its length
/// through `out_len`; free it with lilia_c_free_bytes. Null only past 4 GiB.
uint8_t *lilia_c_execute(const LiliaHandle *handle, const uint8_t *request, size_t len,
                         int encoding, size_t *out_len);

/// Closes the database once its running calls return (waiting at most `timeout_ms`) and frees
/// the handle. 0 on success; -1 with the error's JSON through `out_err`.
int lilia_c_close(LiliaHandle *handle, uint64_t timeout_ms, char **out_err);

/// Frees a frame from lilia_c_execute.
void lilia_c_free_bytes(uint8_t *bytes, size_t len);

/// Frees an error string from lilia_c_open or lilia_c_close.
void lilia_c_free_string(char *message);

#ifdef __cplusplus
}
#endif
