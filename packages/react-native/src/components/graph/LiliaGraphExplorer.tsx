import React, { useState, useCallback } from 'react';
import {
  View,
  Text,
  TouchableOpacity,
  ScrollView,
  ActivityIndicator,
  StyleSheet,
  type StyleProp,
  type ViewStyle,
} from 'react-native';
import { useGraphNeighbors } from '../../hooks.js';
import type { GraphDirection } from '../../types.js';
import type { LiliaDatabase } from '../../database.js';

export interface LiliaGraphExplorerProps {
  graph: string;
  rootNode: string;
  direction?: GraphDirection;
  kindFilter?: string;
  renderNode?: (nodeId: string, isCurrent: boolean) => React.ReactNode;
  onNodePress?: (nodeId: string) => void;
  containerStyle?: StyleProp<ViewStyle>;
  db?: LiliaDatabase | null;
}

/**
 * Idiomatic mobile Graph explorer allowing interactive traversal of node neighbors and relations.
 */
export function LiliaGraphExplorer({
  graph,
  rootNode,
  direction = 'out',
  kindFilter,
  renderNode,
  onNodePress,
  containerStyle,
  db,
}: LiliaGraphExplorerProps): React.ReactElement {
  const [currentNode, setCurrentNode] = useState<string>(rootNode);
  const [history, setHistory] = useState<string[]>([]);

  const { neighbors, isLoading, error } = useGraphNeighbors(graph, currentNode, {
    direction,
    kindFilter,
    db,
  });

  const handleSelectNeighbor = useCallback(
    (neighbor: string) => {
      setHistory((prev) => [...prev, currentNode]);
      setCurrentNode(neighbor);
      onNodePress?.(neighbor);
    },
    [currentNode, onNodePress]
  );

  const handleBack = useCallback(() => {
    if (history.length === 0) return;
    const prev = history[history.length - 1];
    setHistory((h) => h.slice(0, -1));
    setCurrentNode(prev);
    onNodePress?.(prev);
  }, [history, onNodePress]);

  const handleReset = useCallback(() => {
    setHistory([]);
    setCurrentNode(rootNode);
    onNodePress?.(rootNode);
  }, [rootNode, onNodePress]);

  return (
    <View style={[styles.container, containerStyle]}>
      {/* Header with Navigation Controls */}
      <View style={styles.header}>
        <View style={styles.navRow}>
          {history.length > 0 && (
            <TouchableOpacity onPress={handleBack} style={styles.navButton}>
              <Text style={styles.navButtonText}>← Back</Text>
            </TouchableOpacity>
          )}
          {currentNode !== rootNode && (
            <TouchableOpacity onPress={handleReset} style={styles.navButton}>
              <Text style={styles.navButtonText}>Reset to Root</Text>
            </TouchableOpacity>
          )}
        </View>

        {/* Current Node Display */}
        {renderNode ? (
          renderNode(currentNode, true)
        ) : (
          <View style={styles.currentNodeCard}>
            <Text style={styles.currentNodeLabel}>Current Node</Text>
            <Text style={styles.currentNodeId}>{currentNode}</Text>
          </View>
        )}
      </View>

      {/* Neighbors List */}
      <View style={styles.neighborsSection}>
        <Text style={styles.neighborsTitle}>
          Neighbors ({neighbors.length})
        </Text>

        {isLoading ? (
          <ActivityIndicator style={{ marginTop: 20 }} size="small" color="#3B82F6" />
        ) : error ? (
          <Text style={styles.errorText}>Error: {error.message}</Text>
        ) : neighbors.length === 0 ? (
          <Text style={styles.emptyText}>No connected neighbors</Text>
        ) : (
          <ScrollView horizontal showsHorizontalScrollIndicator={false} style={styles.scroll}>
            {neighbors.map((neighbor) => (
              <TouchableOpacity
                key={neighbor}
                style={styles.neighborCard}
                onPress={() => handleSelectNeighbor(neighbor)}
                activeOpacity={0.7}
              >
                {renderNode ? (
                  renderNode(neighbor, false)
                ) : (
                  <>
                    <Text style={styles.neighborId}>{neighbor}</Text>
                    <Text style={styles.exploreText}>Explore →</Text>
                  </>
                )}
              </TouchableOpacity>
            ))}
          </ScrollView>
        )}
      </View>
    </View>
  );
}

const styles = StyleSheet.create({
  container: {
    backgroundColor: '#FFFFFF',
    borderRadius: 12,
    padding: 16,
    shadowColor: '#000',
    shadowOpacity: 0.05,
    shadowRadius: 8,
    elevation: 2,
  },
  header: {
    marginBottom: 16,
  },
  navRow: {
    flexDirection: 'row',
    justifyContent: 'space-between',
    marginBottom: 8,
  },
  navButton: {
    paddingVertical: 4,
    paddingHorizontal: 8,
    backgroundColor: '#F3F4F6',
    borderRadius: 6,
  },
  navButtonText: {
    fontSize: 12,
    color: '#4B5563',
    fontWeight: '600',
  },
  currentNodeCard: {
    backgroundColor: '#EFF6FF',
    borderColor: '#BFDBFE',
    borderWidth: 1,
    borderRadius: 10,
    padding: 12,
    alignItems: 'center',
  },
  currentNodeLabel: {
    fontSize: 11,
    color: '#3B82F6',
    fontWeight: '700',
    textTransform: 'uppercase',
  },
  currentNodeId: {
    fontSize: 18,
    fontWeight: '700',
    color: '#1E40AF',
    marginTop: 2,
  },
  neighborsSection: {
    marginTop: 4,
  },
  neighborsTitle: {
    fontSize: 13,
    fontWeight: '700',
    color: '#6B7280',
    marginBottom: 8,
    textTransform: 'uppercase',
  },
  scroll: {
    flexDirection: 'row',
  },
  neighborCard: {
    backgroundColor: '#F9FAFB',
    borderColor: '#E5E7EB',
    borderWidth: 1,
    borderRadius: 8,
    padding: 12,
    marginRight: 10,
    minWidth: 100,
    alignItems: 'center',
  },
  neighborId: {
    fontSize: 14,
    fontWeight: '600',
    color: '#111827',
  },
  exploreText: {
    fontSize: 11,
    color: '#3B82F6',
    fontWeight: '600',
    marginTop: 4,
  },
  emptyText: {
    fontSize: 13,
    color: '#9CA3AF',
    fontStyle: 'italic',
    paddingVertical: 12,
  },
  errorText: {
    color: '#EF4444',
    fontSize: 13,
  },
});
