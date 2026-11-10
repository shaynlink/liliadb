import React, { useState, useEffect } from 'react';
import {
  View,
  TextInput,
  TouchableOpacity,
  Text,
  ActivityIndicator,
  StyleSheet,
  type StyleProp,
  type ViewStyle,
  type TextStyle,
} from 'react-native';
import { useFts } from '../../hooks.js';
import type { FtsHit } from '../../types.js';
import type { LiliaDatabase } from '../../database.js';

export interface LiliaSearchBarProps {
  collection: string;
  onSearch?: (hits: FtsHit[]) => void;
  limit?: number;
  debounceMs?: number;
  placeholder?: string;
  containerStyle?: StyleProp<ViewStyle>;
  inputStyle?: StyleProp<TextStyle>;
  db?: LiliaDatabase | null;
  onClear?: () => void;
}

/**
 * Idiomatic full-text search input connected directly to LiliaDB FTS5.
 * Features built-in debouncing, clear button, and live search activity spinner.
 */
export function LiliaSearchBar({
  collection,
  onSearch,
  limit = 20,
  debounceMs = 250,
  placeholder = 'Search...',
  containerStyle,
  inputStyle,
  db,
  onClear,
}: LiliaSearchBarProps): React.ReactElement {
  const [query, setQuery] = useState('');
  const { hits, isSearching } = useFts(collection, query, {
    limit,
    debounceMs,
    db,
  });

  useEffect(() => {
    onSearch?.(hits);
  }, [hits, onSearch]);

  const handleClear = () => {
    setQuery('');
    onClear?.();
  };

  return (
    <View style={[styles.container, containerStyle]}>
      <Text style={styles.icon}>🔍</Text>
      <TextInput
        style={[styles.input, inputStyle]}
        value={query}
        onChangeText={setQuery}
        placeholder={placeholder}
        placeholderTextColor="#9CA3AF"
        returnKeyType="search"
        autoCapitalize="none"
        autoCorrect={false}
      />
      {isSearching ? (
        <ActivityIndicator size="small" color="#6B7280" style={styles.actionIcon} />
      ) : query.length > 0 ? (
        <TouchableOpacity onPress={handleClear} style={styles.actionIcon} hitSlop={{ top: 8, bottom: 8, left: 8, right: 8 }}>
          <Text style={styles.clearText}>✕</Text>
        </TouchableOpacity>
      ) : null}
    </View>
  );
}

const styles = StyleSheet.create({
  container: {
    flexDirection: 'row',
    alignItems: 'center',
    backgroundColor: '#F3F4F6',
    borderRadius: 10,
    paddingHorizontal: 12,
    height: 44,
  },
  icon: {
    fontSize: 16,
    marginRight: 8,
  },
  input: {
    flex: 1,
    fontSize: 16,
    color: '#111827',
    paddingVertical: 0,
  },
  actionIcon: {
    marginLeft: 8,
  },
  clearText: {
    fontSize: 14,
    color: '#6B7280',
    fontWeight: '700',
  },
});
