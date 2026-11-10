import React, { useState } from 'react';
import {
  TouchableOpacity,
  Text,
  Share,
  ActivityIndicator,
  Platform,
  StyleSheet,
  type StyleProp,
  type ViewStyle,
  type TextStyle,
} from 'react-native';
import { getDefaultDatabase, type LiliaDatabase } from '../../database.js';
import { useDatabase } from '../../context.js';

export interface LiliaBackupShareButtonProps {
  title?: string;
  fileName?: string;
  buttonStyle?: StyleProp<ViewStyle>;
  textStyle?: StyleProp<TextStyle>;
  onBackupComplete?: (path: string) => void;
  onError?: (error: Error) => void;
  db?: LiliaDatabase | null;
}

/**
 * Idiomatic mobile backup button performing a hot consistent snapshot of LiliaDB
 * and immediately opening the native system share sheet (AirDrop, WhatsApp, Google Drive, Files).
 */
export function LiliaBackupShareButton({
  title = 'Export Backup',
  fileName,
  buttonStyle,
  textStyle,
  onBackupComplete,
  onError,
  db: explicitDb,
}: LiliaBackupShareButtonProps): React.ReactElement {
  const [isExporting, setIsExporting] = useState(false);

  let db: LiliaDatabase | null = null;
  try {
    db = useDatabase(explicitDb);
  } catch {
    db = explicitDb ?? getDefaultDatabase();
  }

  const handleExport = async () => {
    if (!db) {
      onError?.(new Error('[LiliaDB] No database instance found to backup'));
      return;
    }

    setIsExporting(true);
    try {
      const name = fileName ?? `lilia_backup_${Date.now()}.db`;
      const backupPath = `${db.path}.backup_${Date.now()}.db`;

      await db.maintenance.backup(backupPath);
      onBackupComplete?.(backupPath);

      // Open Native Share Sheet
      await Share.share(
        Platform.OS === 'ios'
          ? {
              url: backupPath,
              title: name,
            }
          : {
              title: name,
              message: `LiliaDB Database Backup: ${backupPath}`,
            }
      );
    } catch (err) {
      const error = err instanceof Error ? err : new Error(String(err));
      onError?.(error);
    } finally {
      setIsExporting(false);
    }
  };

  return (
    <TouchableOpacity
      style={[styles.button, buttonStyle]}
      onPress={handleExport}
      disabled={isExporting}
      activeOpacity={0.7}
    >
      {isExporting ? (
        <ActivityIndicator size="small" color="#FFFFFF" />
      ) : (
        <Text style={[styles.text, textStyle]}>📦 {title}</Text>
      )}
    </TouchableOpacity>
  );
}

const styles = StyleSheet.create({
  button: {
    backgroundColor: '#059669',
    paddingVertical: 12,
    paddingHorizontal: 20,
    borderRadius: 8,
    alignItems: 'center',
    justifyContent: 'center',
    flexDirection: 'row',
  },
  text: {
    color: '#FFFFFF',
    fontSize: 15,
    fontWeight: '600',
  },
});
