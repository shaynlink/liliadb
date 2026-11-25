// Where a database lives on the device.

import { LiliaError } from "@liliadb/client";
import { Platform } from "react-native";
import { platformModule } from "./native.js";
import type { OpenDatabaseOptions } from "./types.js";

function invalid(message: string): LiliaError {
  return new LiliaError({ code: "INVALID_INPUT", message, retryable: false });
}

/**
 * The directory of the database `options` describe: `path` when given, else `<name>.lilia` in
 * the directory the platform options choose (Application Support on iOS, the no-backup files
 * directory on Android, by default).
 */
export function resolveDatabasePath(
  options: Pick<OpenDatabaseOptions, "name" | "path" | "ios" | "android">,
): string {
  if (options.path !== undefined) {
    if (options.path.trim() === "") throw invalid("path is empty");
    return options.path;
  }
  const name = options.name?.trim() || "app_data";
  const directoryName = name.endsWith(".lilia") ? name : `${name}.lilia`;
  const native = platformModule();
  let directory: string | null | undefined;
  if (Platform.OS === "ios") {
    const group = options.ios?.groupContainerIdentifier;
    directory =
      group === undefined
        ? native.directories()[options.ios?.directory ?? "applicationSupport"]
        : native.groupContainer(group);
    if (!directory)
      throw invalid(
        group === undefined ? "no such iOS directory" : `the app is not in the App Group ${group}`,
      );
  } else if (Platform.OS === "android") {
    directory = native.directories()[options.android?.directory ?? "noBackup"];
    if (!directory) throw invalid("no such Android directory");
  } else {
    throw invalid(`no default directory on ${Platform.OS}: pass a path`);
  }
  return `${directory}/${directoryName}`;
}
