// The C ABI of the LiliaDB engine (crates/lilia-ffi/src/c_abi.rs), which the plugin's Dart code
// calls through dart:ffi (lib/src/ffi/bindings.dart).

#ifndef LILIADB_FLUTTER_H
#define LILIADB_FLUTTER_H

#include <stddef.h>
#include <stdint.h>

typedef struct LiliaHandle LiliaHandle;

LiliaHandle *lilia_c_open(const char *options_json, char **out_err);
uint8_t *lilia_c_execute(const LiliaHandle *handle, const uint8_t *request, size_t len,
                         int encoding, size_t *out_len);
int lilia_c_close(LiliaHandle *handle, uint64_t timeout_ms, char **out_err);
void lilia_c_free_bytes(uint8_t *bytes, size_t len);
void lilia_c_free_string(char *message);

#endif
