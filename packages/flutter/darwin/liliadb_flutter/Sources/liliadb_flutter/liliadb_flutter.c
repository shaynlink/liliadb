// Dart finds the engine's functions at run time, in the process (DynamicLibrary.process()), and
// no native code calls them: without a reference, the linker would leave the static library of
// the xcframework out. This table references each function of the C ABI, so the engine is
// linked into the plugin and its functions stay global symbols.

#include "liliadb_flutter.h"

__attribute__((used)) static const void *const liliadb_flutter_exports[] = {
    (const void *)lilia_c_open,
    (const void *)lilia_c_execute,
    (const void *)lilia_c_close,
    (const void *)lilia_c_free_bytes,
    (const void *)lilia_c_free_string,
};
