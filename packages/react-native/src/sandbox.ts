import { Platform } from 'react-native';
import type { OpenDatabaseOptions } from './types.js';

/**
 * Resolves the optimal sandbox file path for the database based on the host OS.
 *
 * - iOS: Defaults to Application Support (not purgeable, excluded from iCloud if requested).
 * - Android: Defaults to noBackupFilesDir (avoids saturating Google Drive backup quota).
 */
export function resolveDatabasePath(options: OpenDatabaseOptions): string {
  if (options.path && options.path.trim().length > 0) {
    return options.path;
  }

  const name = options.name?.trim() || 'app_data';
  const cleanName = name.endsWith('.lilia') ? name : `${name}.lilia`;

  if (Platform.OS === 'ios') {
    if (options.ios?.groupContainerIdentifier) {
      // Shared container for iOS App Groups (Widgets, Watch apps)
      return `group://${options.ios.groupContainerIdentifier}/${cleanName}`;
    }
    const dir = options.ios?.directory ?? 'applicationSupport';
    return `ios://${dir}/${cleanName}`;
  }

  if (Platform.OS === 'android') {
    const dir = options.android?.directory ?? 'noBackup';
    return `android://${dir}/${cleanName}`;
  }

  return cleanName;
}
