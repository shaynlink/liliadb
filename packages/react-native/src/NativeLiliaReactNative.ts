// The TurboModule of LiliaDB (codegen spec). Creating it installs the JSI binding,
// `globalThis.__liliadb` (cpp/LiliaJsi.h); its methods name the directories a database may
// live in.

import { TurboModuleRegistry, type TurboModule } from "react-native";

export interface Spec extends TurboModule {
  /**
   * The directories of the app, by name, each created if missing: `applicationSupport`,
   * `documents`, `caches` on iOS; `noBackup`, `files`, `cache` on Android.
   */
  directories(): object;
  /** The container of an App Group (iOS), or null when the app is not in it (or on Android). */
  groupContainer(identifier: string): string | null;
  /** Excludes `path` from iCloud backups (iOS); whether it did. */
  excludeFromBackup(path: string): boolean;
}

/** The module; null when the app does not link it. */
export default TurboModuleRegistry.get<Spec>("LiliaReactNative");
