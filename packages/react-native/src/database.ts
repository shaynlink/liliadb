// A LiliaDB database in a React Native app: the calls of `@liliadb/client` over the JSI binding,
// with change notifications for hooks and a checkpoint when the app goes to the background.

import { Database, nativeError, type CloseOptions } from "@liliadb/client";
import { AppState, Platform, type NativeEventSubscription } from "react-native";
import { changedTopics } from "./changes.js";
import { nativeLilia, platformModule } from "./native.js";
import { DatabaseObserver } from "./observer.js";
import { resolveDatabasePath } from "./paths.js";
import { JsiTransport } from "./transport.js";
import type { OpenDatabaseOptions } from "./types.js";

declare global {
  // The databases open in development, by path: they outlive Fast Refresh.
  var __LILIA_DEV_DATABASES__: Map<string, LiliaDatabase> | undefined;
}

/** Whether the app runs in development (Metro's `__DEV__`). */
function isDevelopment(): boolean {
  return (globalThis as { __DEV__?: boolean }).__DEV__ === true;
}

export class LiliaDatabase extends Database {
  private readonly appState?: NativeEventSubscription;

  private constructor(
    transport: JsiTransport,
    /** The database's directory. */
    readonly path: string,
    private readonly observer: DatabaseObserver,
    checkpointOnBackground: boolean
  ) {
    super(transport);
    if (checkpointOnBackground) {
      this.appState = AppState.addEventListener("change", (state) => {
        if (state === "background" && !this.isClosed) {
          this.checkpoint().catch((error: unknown) => console.warn("[LiliaDB] background checkpoint failed", error));
        }
      });
    }
  }

  /**
   * Opens (or creates) the database `options` describe, by default `app_data.lilia` in the
   * platform's directory for app data (see `resolveDatabasePath`).
   */
  static async open(options: OpenDatabaseOptions = {}): Promise<LiliaDatabase> {
    const path = resolveDatabasePath(options);
    const reuse = isDevelopment() && (options.dev?.reuseAcrossFastRefresh ?? true);
    const open = reuse ? globalThis.__LILIA_DEV_DATABASES__?.get(path) : undefined;
    if (open && !open.isClosed) return open;

    let native;
    try {
      native = await nativeLilia().open(
        JSON.stringify({
          path,
          durability: options.durability,
          profile: options.profile ?? "mobile",
          ledger: options.ledger,
          history_retention_ms: options.historyRetentionMs,
          undo_depth: options.undoDepth,
        })
      );
    } catch (error) {
      throw nativeError(error);
    }
    if (Platform.OS === "ios" && (options.ios?.excludeFromCloudBackup ?? true)) {
      platformModule().excludeFromBackup(path);
    }
    const observer = new DatabaseObserver();
    const transport = new JsiTransport(native, (operation) => observer.emit(changedTopics(operation)));
    const database = new LiliaDatabase(transport, path, observer, options.checkpointOnBackground ?? true);
    if (reuse) (globalThis.__LILIA_DEV_DATABASES__ ??= new Map()).set(path, database);
    return database;
  }

  /**
   * Calls `listener` after each write of this database that changes one of `topics` (see
   * `topics`; `"*"` for every write). Returns the unsubscribe.
   */
  subscribe(topics: string | readonly string[], listener: () => void): () => void {
    return this.observer.subscribe(topics, listener);
  }

  override async close(options?: CloseOptions): Promise<void> {
    this.appState?.remove();
    if (globalThis.__LILIA_DEV_DATABASES__?.get(this.path) === this) globalThis.__LILIA_DEV_DATABASES__.delete(this.path);
    try {
      await super.close(options);
    } finally {
      this.observer.clear();
    }
  }
}
