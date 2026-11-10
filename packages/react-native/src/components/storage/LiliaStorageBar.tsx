import React, { useState } from 'react';
import {
  View,
  Text,
  TouchableOpacity,
  ActivityIndicator,
  StyleSheet,
  type StyleProp,
  type ViewStyle,
} from 'react-native';
import { useDatabaseMaintenance } from '../../hooks.js';
import type { LiliaDatabase } from '../../database.js';

export interface LiliaStorageBarProps {
  showVacuumButton?: boolean;
  showCheckpointButton?: boolean;
  barHeight?: number;
  style?: StyleProp<ViewStyle>;
  db?: LiliaDatabase | null;
  onMaintenanceComplete?: () => void;
}

function formatBytes(bytes: number): string {
  if (bytes <= 0) return '0 B';
  const k = 1024;
  const sizes = ['B', 'KB', 'MB', 'GB'];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return `${(bytes / Math.pow(k, i)).toFixed(1)} ${sizes[i]}`;
}

/**
 * Idiomatic storage gauge and maintenance component for LiliaDB.
 * Visualizes storage distribution across main database, WAL journal, and SHM shared memory,
 * with one-tap native Vacuum and Checkpoint operations.
 */
export function LiliaStorageBar({
  showVacuumButton = true,
  showCheckpointButton = true,
  barHeight = 10,
  style,
  db,
  onMaintenanceComplete,
}: LiliaStorageBarProps): React.ReactElement {
  const { metrics, isLoading, vacuum, checkpoint } = useDatabaseMaintenance(db);
  const [isOperating, setIsOperating] = useState(false);

  const mainBytes = metrics?.mainBytes ?? 0;
  const walBytes = metrics?.walBytes ?? 0;
  const shmBytes = metrics?.shmBytes ?? 0;
  const totalBytes = metrics?.totalBytes || (mainBytes + walBytes + shmBytes) || 1;

  const mainPct = Math.round((mainBytes / totalBytes) * 100);
  const walPct = Math.round((walBytes / totalBytes) * 100);
  const shmPct = Math.max(0, 100 - mainPct - walPct);

  const handleVacuum = async () => {
    setIsOperating(true);
    try {
      await vacuum();
      onMaintenanceComplete?.();
    } finally {
      setIsOperating(false);
    }
  };

  const handleCheckpoint = async () => {
    setIsOperating(true);
    try {
      await checkpoint('truncate');
      onMaintenanceComplete?.();
    } finally {
      setIsOperating(false);
    }
  };

  return (
    <View style={[styles.container, style]}>
      <View style={styles.header}>
        <Text style={styles.title}>LiliaDB Storage</Text>
        <Text style={styles.total}>{formatBytes(metrics?.totalBytes ?? 0)}</Text>
      </View>

      {/* Segmented Progress Bar */}
      <View style={[styles.barContainer, { height: barHeight }]}>
        <View style={[styles.barSegment, { flex: Math.max(mainPct, 1), backgroundColor: '#3B82F6' }]} />
        {walBytes > 0 && (
          <View style={[styles.barSegment, { flex: Math.max(walPct, 1), backgroundColor: '#F59E0B' }]} />
        )}
        {shmBytes > 0 && (
          <View style={[styles.barSegment, { flex: Math.max(shmPct, 1), backgroundColor: '#10B981' }]} />
        )}
      </View>

      {/* Legend */}
      <View style={styles.legend}>
        <View style={styles.legendItem}>
          <View style={[styles.dot, { backgroundColor: '#3B82F6' }]} />
          <Text style={styles.legendText}>Data: {formatBytes(mainBytes)}</Text>
        </View>
        <View style={styles.legendItem}>
          <View style={[styles.dot, { backgroundColor: '#F59E0B' }]} />
          <Text style={styles.legendText}>WAL: {formatBytes(walBytes)}</Text>
        </View>
        <View style={styles.legendItem}>
          <View style={[styles.dot, { backgroundColor: '#10B981' }]} />
          <Text style={styles.legendText}>SHM: {formatBytes(shmBytes)}</Text>
        </View>
      </View>

      {/* Maintenance Actions */}
      {(showVacuumButton || showCheckpointButton) && (
        <View style={styles.actions}>
          {showCheckpointButton && (
            <TouchableOpacity
              style={styles.actionButton}
              onPress={handleCheckpoint}
              disabled={isOperating || isLoading}
            >
              <Text style={styles.actionButtonText}>Checkpoint WAL</Text>
            </TouchableOpacity>
          )}

          {showVacuumButton && (
            <TouchableOpacity
              style={[styles.actionButton, styles.vacuumButton]}
              onPress={handleVacuum}
              disabled={isOperating || isLoading}
            >
              <Text style={[styles.actionButtonText, styles.vacuumButtonText]}>Vacuum DB</Text>
            </TouchableOpacity>
          )}

          {isOperating && <ActivityIndicator size="small" color="#3B82F6" style={{ marginLeft: 8 }} />}
        </View>
      )}
    </View>
  );
}

const styles = StyleSheet.create({
  container: {
    backgroundColor: '#FFFFFF',
    borderRadius: 12,
    padding: 16,
    borderWidth: 1,
    borderColor: '#E5E7EB',
  },
  header: {
    flexDirection: 'row',
    justifyContent: 'space-between',
    alignItems: 'center',
    marginBottom: 8,
  },
  title: {
    fontSize: 14,
    fontWeight: '700',
    color: '#111827',
  },
  total: {
    fontSize: 14,
    fontWeight: '700',
    color: '#3B82F6',
  },
  barContainer: {
    flexDirection: 'row',
    borderRadius: 6,
    overflow: 'hidden',
    backgroundColor: '#F3F4F6',
    marginVertical: 8,
  },
  barSegment: {
    height: '100%',
  },
  legend: {
    flexDirection: 'row',
    justifyContent: 'space-between',
    marginTop: 4,
    marginBottom: 12,
  },
  legendItem: {
    flexDirection: 'row',
    alignItems: 'center',
  },
  dot: {
    width: 8,
    height: 8,
    borderRadius: 4,
    marginRight: 4,
  },
  legendText: {
    fontSize: 11,
    color: '#6B7280',
    fontWeight: '500',
  },
  actions: {
    flexDirection: 'row',
    alignItems: 'center',
    borderTopWidth: 1,
    borderTopColor: '#F3F4F6',
    paddingTop: 12,
  },
  actionButton: {
    backgroundColor: '#F3F4F6',
    paddingVertical: 6,
    paddingHorizontal: 12,
    borderRadius: 6,
    marginRight: 8,
  },
  actionButtonText: {
    fontSize: 12,
    fontWeight: '600',
    color: '#374151',
  },
  vacuumButton: {
    backgroundColor: '#EFF6FF',
  },
  vacuumButtonText: {
    color: '#2563EB',
  },
});
