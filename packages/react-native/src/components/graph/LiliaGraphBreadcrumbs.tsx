import React from 'react';
import {
  View,
  Text,
  TouchableOpacity,
  ScrollView,
  StyleSheet,
  type StyleProp,
  type ViewStyle,
  type TextStyle,
} from 'react-native';

export interface LiliaGraphBreadcrumbsProps {
  path: string[];
  onNodePress?: (nodeId: string, index: number) => void;
  separator?: string;
  containerStyle?: StyleProp<ViewStyle>;
  nodeStyle?: StyleProp<TextStyle>;
  separatorStyle?: StyleProp<TextStyle>;
  activeNodeStyle?: StyleProp<TextStyle>;
}

/**
 * Idiomatic breadcrumb navigation trail for Graph trees and hierarchies in LiliaDB.
 */
export function LiliaGraphBreadcrumbs({
  path,
  onNodePress,
  separator = '›',
  containerStyle,
  nodeStyle,
  separatorStyle,
  activeNodeStyle,
}: LiliaGraphBreadcrumbsProps): React.ReactElement {
  return (
    <ScrollView
      horizontal
      showsHorizontalScrollIndicator={false}
      contentContainerStyle={[styles.container, containerStyle]}
    >
      {path.map((nodeId, index) => {
        const isLast = index === path.length - 1;
        return (
          <View key={`${nodeId}-${index}`} style={styles.segment}>
            <TouchableOpacity
              onPress={() => onNodePress?.(nodeId, index)}
              disabled={isLast && !onNodePress}
              activeOpacity={0.6}
            >
              <Text
                style={[
                  styles.nodeText,
                  nodeStyle,
                  isLast && [styles.activeNodeText, activeNodeStyle],
                ]}
              >
                {nodeId}
              </Text>
            </TouchableOpacity>

            {!isLast && (
              <Text style={[styles.separator, separatorStyle]}>{separator}</Text>
            )}
          </View>
        );
      })}
    </ScrollView>
  );
}

const styles = StyleSheet.create({
  container: {
    flexDirection: 'row',
    alignItems: 'center',
    paddingVertical: 8,
    paddingHorizontal: 12,
  },
  segment: {
    flexDirection: 'row',
    alignItems: 'center',
  },
  nodeText: {
    fontSize: 14,
    color: '#6B7280',
    fontWeight: '500',
  },
  activeNodeText: {
    color: '#111827',
    fontWeight: '700',
  },
  separator: {
    marginHorizontal: 8,
    fontSize: 16,
    color: '#9CA3AF',
  },
});
