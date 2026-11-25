// The native side: the JSI binding (`cpp/LiliaJsi.h`) and the TurboModule that installs it and
// names the app's directories (`ios/LiliaReactNative.mm`, `LiliaReactNativeModule.kt`).

import { LiliaError } from "@liliadb/client";
import LiliaModule from "./NativeLiliaReactNative.js";

/** A database the JSI binding opened. A frame is its JSON text and the bytes after it. */
export interface NativeDatabase {
  execute(request: Uint8Array): Promise<[string, ArrayBuffer]>;
  executeSync(request: Uint8Array): [string, ArrayBuffer];
  close(timeoutMs: number): Promise<void>;
}

/** `globalThis.__liliadb`, which the TurboModule installs. */
export interface NativeLilia {
  /** Opens the database the JSON of `lilia_c_open`'s options describes. */
  open(optionsJson: string): Promise<NativeDatabase>;
}

/** The directories a database may live in, by name; each exists. */
export type Directories = Partial<
  Record<"applicationSupport" | "documents" | "caches" | "noBackup" | "files" | "cache", string>
>;

declare global {
  // The binding's entry point, which the TurboModule installs when it is created.
  var __liliadb: NativeLilia | undefined;
}

/** The TurboModule, whose creation installed the binding. */
export function platformModule(): {
  directories(): Directories;
  groupContainer(identifier: string): string | null;
  excludeFromBackup(path: string): boolean;
} {
  const module = LiliaModule;
  if (!module) {
    throw new LiliaError({
      code: "UNSUPPORTED",
      message:
        "the LiliaDB native module is not linked: run `pod install` (iOS) or rebuild the app (Android)",
      retryable: false,
    });
  }
  return {
    directories: () => module.directories() as Directories,
    groupContainer: (identifier) => module.groupContainer(identifier),
    excludeFromBackup: (path) => module.excludeFromBackup(path),
  };
}

/** The JSI binding. */
export function nativeLilia(): NativeLilia {
  platformModule();
  if (!globalThis.__liliadb) {
    throw new LiliaError({
      code: "UNSUPPORTED",
      message:
        "the LiliaDB JSI binding is not installed: the app must run the New Architecture, with JavaScript on the device",
      retryable: false,
    });
  }
  return globalThis.__liliadb;
}
