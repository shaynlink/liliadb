// The JSI binding of LiliaDB: JavaScript calls the engine's C ABI directly, without the bridge.

#pragma once

#include <ReactCommon/CallInvoker.h>
#include <jsi/jsi.h>

#include <memory>

namespace liliadb {

/// Installs `globalThis.__liliadb` in `runtime`:
///
/// - `open(optionsJson: string): Promise<NativeDatabase>` opens a database (the options of
///   `lilia_c_open`);
/// - `NativeDatabase.execute(request: Uint8Array): Promise<[string, ArrayBuffer]>` runs a
///   MessagePack operation on a worker thread; its JS frame comes back as the JSON text and the
///   bytes after it (the engine's own buffer, not a copy);
/// - `NativeDatabase.executeSync(request: Uint8Array): [string, ArrayBuffer]` runs it on the
///   JavaScript thread;
/// - `NativeDatabase.close(timeoutMs: number): Promise<void>` closes the database.
///
/// A failure rejects (or throws) an `Error` whose message is the JSON of the engine's error.
/// `invoker` brings the results of worker threads back to the JavaScript thread.
void install(facebook::jsi::Runtime &runtime,
             std::shared_ptr<facebook::react::CallInvoker> invoker);

} // namespace liliadb
