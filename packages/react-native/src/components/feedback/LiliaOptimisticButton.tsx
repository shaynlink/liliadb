import React, { useState, useCallback } from "react";
import {
  TouchableOpacity,
  Text,
  ActivityIndicator,
  StyleSheet,
  type StyleProp,
  type ViewStyle,
  type TextStyle,
} from "react-native";

/** The calls of `expo-haptics` this component makes. */
interface Haptics {
  impactAsync(style: unknown): Promise<void>;
  ImpactFeedbackStyle: { Light: unknown };
}

/** `expo-haptics`, when the app installed it. */
const haptics: Haptics | null = (() => {
  try {
    const module = require("expo-haptics") as Partial<Haptics> | null;
    return module?.impactAsync && module.ImpactFeedbackStyle ? (module as Haptics) : null;
  } catch {
    return null;
  }
})();

export interface LiliaOptimisticButtonProps {
  onPress: () => Promise<void> | void;
  title?: string;
  children?: React.ReactNode;
  haptic?: boolean;
  style?: StyleProp<ViewStyle>;
  textStyle?: StyleProp<TextStyle>;
  disabled?: boolean;
  loadingComponent?: React.ReactNode;
}

/**
 * Idiomatic mobile button with optimistic state management and optional haptic feedback.
 */
export function LiliaOptimisticButton({
  onPress,
  title,
  children,
  haptic = true,
  style,
  textStyle,
  disabled = false,
  loadingComponent,
}: LiliaOptimisticButtonProps): React.ReactElement {
  const [isPending, setIsPending] = useState(false);

  const handlePress = useCallback(async () => {
    if (isPending || disabled) return;

    if (haptic && haptics) {
      void haptics.impactAsync(haptics.ImpactFeedbackStyle.Light);
    }

    try {
      setIsPending(true);
      await onPress();
    } finally {
      setIsPending(false);
    }
  }, [isPending, disabled, haptic, onPress]);

  return (
    <TouchableOpacity
      style={[styles.button, isPending && styles.buttonPending, style]}
      onPress={handlePress}
      disabled={disabled || isPending}
      activeOpacity={0.7}
    >
      {isPending ? (
        (loadingComponent ?? <ActivityIndicator size="small" color="#FFFFFF" />)
      ) : children ? (
        children
      ) : (
        <Text style={[styles.text, textStyle]}>{title}</Text>
      )}
    </TouchableOpacity>
  );
}

const styles = StyleSheet.create({
  button: {
    backgroundColor: "#3B82F6",
    paddingVertical: 12,
    paddingHorizontal: 20,
    borderRadius: 8,
    alignItems: "center",
    justifyContent: "center",
    flexDirection: "row",
  },
  buttonPending: {
    opacity: 0.8,
  },
  text: {
    color: "#FFFFFF",
    fontSize: 16,
    fontWeight: "600",
  },
});
