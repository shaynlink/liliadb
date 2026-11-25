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
  showCheckpointButton?: boolean;
  barHeight?: number;
  style?: StyleProp<ViewStyle>;
  db?: LiliaDatabase | null;
  onMaintenanceComplete?: () => void;
}

function formatBytes(bytes: number): string {
  if (bytes <= 0) return '0 B';
  const k = 1024;
  const sizes = ['B', 'KB', 'MB', 'GB', 'TB'];
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(k)), sizes.length - 1);
  return `${(bytes / Math.pow(k, i)).toFixed(1)} ${sizes[i]}`;
}

/**
 * Idiomatic storage gauge for LiliaDB: the bytes of the database's sorted runs, on disk, and of
 * its memtables, in memory until a checkpoint writes them to runs; with a one-tap checkpoint.
 */
export function LiliaStorageBar({
  showCheckpointButton = true,
  barHeight = 10,
  style,
  db,
  onMaintenanceComplete,
}: LiliaStorageBarProps): React.ReactElement {
  const { stats, isLoading, checkpoint } = useDatabaseMaintenance(db);
  const [isOperating, setIsOperating] = useState(false);

  const diskBytes = stats?.levels.reduce((sum, level) => sum + level.bytes, 0) ?? 0;
  const memoryBytes = stats?.memtableBytes ?? 0;
  const runs = stats?.levels.reduce((sum, level) => sum + level.runs, 0) ?? 0;
  const totalBytes = diskBytes + memoryBytes;
  const diskShare = totalBytes === 0 ? 0 : Math.round((diskBytes / totalBytes) * 100);

  const handleCheckpoint = async () => {
    setIsOperating(true);
    try {
      await checkpoint();
      onMaintenanceComplete?.();
    } finally {
      setIsOperating(false);
    }
  };

  return (
    <View style={[styles.container, style]}>
      <View style={styles.header}>
        <Text style={styles.title}>LiliaDB Storage</Text>
        <Text style={styles.total}>{formatBytes(totalBytes)}</Text>
      </View>

      {/* Segmented Progress Bar */}
      <View style={[styles.barContainer, { height: barHeight }]}>
        {diskBytes > 0 && <View style={[styles.barSegment, { flex: Math.max(diskShare, 1), backgroundColor: '#3B82F6' }]} />}
        {memoryBytes > 0 && (
          <View style={[styles.barSegment, { flex: Math.max(100 - diskShare, 1), backgroundColor: '#F59E0B' }]} />
        )}
      </View>

      {/* Legend */}
      <View style={styles.legend}>
        <View style={styles.legendItem}>
          <View style={[styles.dot, { backgroundColor: '#3B82F6' }]} />
          <Text style={styles.legendText}>
            On disk: {formatBytes(diskBytes)} ({runs} {runs === 1 ? 'run' : 'runs'})
          </Text>
        </View>
        <View style={styles.legendItem}>
          <View style={[styles.dot, { backgroundColor: '#F59E0B' }]} />
          <Text style={styles.legendText}>In memory: {formatBytes(memoryBytes)}</Text>
        </View>
      </View>

      {showCheckpointButton && (
        <View style={styles.actions}>
          <TouchableOpacity style={styles.actionButton} onPress={handleCheckpoint} disabled={isOperating || isLoading}>
            <Text style={styles.actionButtonText}>Checkpoint</Text>
          </TouchableOpacity>
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
});
