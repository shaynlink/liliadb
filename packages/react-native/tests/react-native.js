// `react-native` for the tests: the platform module is the one of an iOS app whose directories
// are a temporary directory, and its JSI binding, `globalThis.__liliadb`, runs on the engine
// through the Node-API module, which answers with the same frames as the C ABI.

import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { loadNative } from "@liliadb/node/native";

export const root = mkdtempSync(join(tmpdir(), "lilia-react-native-"));
process.on("exit", () => rmSync(root, { recursive: true, force: true }));

/** A frame as the binding hands it over: its JSON text and the bytes after it. */
function parts(frame) {
  const json = frame.readUInt32LE(0);
  return [
    frame.toString("utf8", 4, 4 + json),
    frame.buffer.slice(frame.byteOffset + 4 + json, frame.byteOffset + frame.length),
  ];
}

function database(handle) {
  const out = Buffer.alloc(1 << 16);
  return {
    execute: async (request) => parts(await handle.execute(request)),
    executeSync: (request) => {
      const written = handle.executeSync(request, out);
      return parts(typeof written === "number" ? out.subarray(0, written) : written);
    },
    close: (timeoutMs) => handle.close(timeoutMs),
  };
}

const binding = {
  async open(optionsJson) {
    const options = JSON.parse(optionsJson);
    const handle = await loadNative().open({
      path: options.path,
      durability: options.durability,
      profile: options.profile,
      ledger: options.ledger,
      historyRetentionMs: options.history_retention_ms,
      undoDepth: options.undo_depth,
    });
    return database(handle);
  },
};

const directories = Object.fromEntries(
  ["applicationSupport", "documents", "caches"].map((name) => {
    const path = join(root, name);
    mkdirSync(path, { recursive: true });
    return [name, path];
  }),
);

/** The paths `excludeFromBackup` received. */
export const excludedFromBackup = [];

const module = {
  directories: () => directories,
  groupContainer: (identifier) => (identifier === "group.test" ? directories.documents : null),
  excludeFromBackup: (path) => {
    excludedFromBackup.push(path);
    return true;
  },
};

export const TurboModuleRegistry = {
  get(name) {
    if (name !== "LiliaReactNative") return null;
    globalThis.__liliadb = binding;
    return module;
  },
};

export const Platform = { OS: "ios", select: (choices) => choices.ios ?? choices.default };

const appStateListeners = new Set();
export const AppState = {
  addEventListener(type, listener) {
    appStateListeners.add(listener);
    return { remove: () => appStateListeners.delete(listener) };
  },
  /** Moves the app to `state`, as the system would. */
  emit(state) {
    // A snapshot: a listener may remove itself while they run.
    for (const listener of Array.from(appStateListeners)) listener(state);
  },
};

const component = (name) =>
  Object.assign(({ children }) => children ?? null, { displayName: name });

export const View = component("View");
export const Text = component("Text");
export const TextInput = component("TextInput");
export const TouchableOpacity = component("TouchableOpacity");
export const Switch = component("Switch");
export const Modal = component("Modal");
export const ScrollView = component("ScrollView");
export const SafeAreaView = component("SafeAreaView");
export const ActivityIndicator = component("ActivityIndicator");
export const FlatList = component("FlatList");
export const SectionList = component("SectionList");
export const RefreshControl = component("RefreshControl");
export const StyleSheet = { create: (styles) => styles };
