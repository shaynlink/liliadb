// What the tests read of their `react-native` (see react-native.js).

/** The temporary directory the app's directories are in. */
export const root: string;
/** The paths the platform module excluded from backups. */
export const excludedFromBackup: string[];
export const AppState: {
  /** Moves the app to `state`, as the system would. */
  emit(state: "active" | "background" | "inactive"): void;
};
