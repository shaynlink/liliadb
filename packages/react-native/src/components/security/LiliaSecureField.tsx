import React, { useState, useEffect, useRef, useCallback } from 'react';
import {
  View,
  Text,
  TouchableOpacity,
  StyleSheet,
  type StyleProp,
  type ViewStyle,
  type TextStyle,
} from 'react-native';

export interface LiliaSecureFieldProps {
  value: string;
  maskChar?: string;
  autoHideTimeoutMs?: number;
  containerStyle?: StyleProp<ViewStyle>;
  textStyle?: StyleProp<TextStyle>;
  buttonStyle?: StyleProp<ViewStyle>;
  renderToggleIcon?: (revealed: boolean) => React.ReactNode;
  onReveal?: (revealed: boolean) => void;
}

/**
 * Idiomatic masked security field with one-tap reveal and auto-hide timer.
 * Prevents shoulder surfing for private keys, tokens, and sensitive values stored in LiliaDB.
 */
export function LiliaSecureField({
  value,
  maskChar = '•',
  autoHideTimeoutMs = 8000,
  containerStyle,
  textStyle,
  buttonStyle,
  renderToggleIcon,
  onReveal,
}: LiliaSecureFieldProps): React.ReactElement {
  const [isRevealed, setIsRevealed] = useState(false);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const clearTimer = useCallback(() => {
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  useEffect(() => {
    return clearTimer;
  }, [clearTimer]);

  const toggleReveal = useCallback(() => {
    clearTimer();

    setIsRevealed((prev) => {
      const next = !prev;
      onReveal?.(next);

      if (next && autoHideTimeoutMs > 0) {
        timerRef.current = setTimeout(() => {
          setIsRevealed(false);
          onReveal?.(false);
        }, autoHideTimeoutMs);
      }

      return next;
    });
  }, [clearTimer, autoHideTimeoutMs, onReveal]);

  const masked = value ? maskChar.repeat(Math.min(value.length, 24)) : '';
  const displayText = isRevealed ? value : masked;

  return (
    <View style={[styles.container, containerStyle]}>
      <Text style={[styles.text, textStyle]} numberOfLines={1} ellipsizeMode="middle">
        {displayText}
      </Text>

      <TouchableOpacity
        style={[styles.toggleButton, buttonStyle]}
        onPress={toggleReveal}
        activeOpacity={0.7}
        hitSlop={{ top: 8, bottom: 8, left: 8, right: 8 }}
      >
        {renderToggleIcon ? (
          renderToggleIcon(isRevealed)
        ) : (
          <Text style={styles.toggleText}>{isRevealed ? 'Hide' : 'Show'}</Text>
        )}
      </TouchableOpacity>
    </View>
  );
}

const styles = StyleSheet.create({
  container: {
    flexDirection: 'row',
    alignItems: 'center',
    justifyContent: 'space-between',
    backgroundColor: '#F3F4F6',
    borderRadius: 8,
    paddingHorizontal: 12,
    paddingVertical: 10,
  },
  text: {
    flex: 1,
    fontSize: 15,
    fontFamily: 'monospace',
    color: '#111827',
    marginRight: 8,
  },
  toggleButton: {
    paddingHorizontal: 8,
    paddingVertical: 4,
    backgroundColor: '#E5E7EB',
    borderRadius: 6,
  },
  toggleText: {
    fontSize: 12,
    fontWeight: '600',
    color: '#374151',
  },
});
