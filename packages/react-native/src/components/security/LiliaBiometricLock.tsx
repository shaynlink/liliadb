import React, { useState, useEffect, useCallback } from "react";
import {
  View,
  Text,
  TouchableOpacity,
  StyleSheet,
  type StyleProp,
  type ViewStyle,
} from "react-native";

/** The calls of `expo-local-authentication` this component makes. */
interface LocalAuthentication {
  hasHardwareAsync(): Promise<boolean>;
  isEnrolledAsync(): Promise<boolean>;
  authenticateAsync(options: {
    promptMessage: string;
    fallbackLabel: string;
  }): Promise<{ success: boolean; error?: string }>;
}

/** `expo-local-authentication`, when the app installed it. */
const LocalAuth: LocalAuthentication | null = (() => {
  try {
    const module = require("expo-local-authentication") as Partial<LocalAuthentication> | null;
    return typeof module?.authenticateAsync === "function" ? (module as LocalAuthentication) : null;
  } catch {
    return null;
  }
})();

export interface LiliaBiometricLockProps {
  promptMessage?: string;
  fallbackComponent?: React.ReactNode | ((authenticate: () => Promise<void>) => React.ReactNode);
  children: React.ReactNode;
  autoPrompt?: boolean;
  containerStyle?: StyleProp<ViewStyle>;
}

/**
 * Idiomatic hardware security component requiring Face ID / Touch ID / BiometricPrompt
 * before unlocking and revealing its child components.
 * Dynamically uses `expo-local-authentication` if present.
 */
export function LiliaBiometricLock({
  promptMessage = "Unlock to view protected data",
  fallbackComponent,
  children,
  autoPrompt = true,
  containerStyle,
}: LiliaBiometricLockProps): React.ReactElement {
  // Without `expo-local-authentication` (a mock environment), there is nothing to unlock with.
  const [isUnlocked, setIsUnlocked] = useState(LocalAuth === null);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);

  const authenticate = useCallback(async () => {
    if (!LocalAuth) return;
    try {
      const hasHardware = await LocalAuth.hasHardwareAsync();
      const isEnrolled = await LocalAuth.isEnrolledAsync();

      if (!hasHardware || !isEnrolled) {
        // No hardware biometrics enrolled, unlock or allow access
        setIsUnlocked(true);
        return;
      }

      const result = await LocalAuth.authenticateAsync({
        promptMessage,
        fallbackLabel: "Use Device Passcode",
      });

      if (result.success) {
        setIsUnlocked(true);
        setErrorMessage(null);
      } else {
        setErrorMessage(result.error ?? "Authentication failed");
      }
    } catch {
      // The platform could not prompt (no biometric service): access is not gated.
      setIsUnlocked(true);
    }
  }, [promptMessage]);

  useEffect(() => {
    if (autoPrompt) {
      // `authenticate` sets state only once the platform answered, after its `await`s.
      // oxlint-disable-next-line react/set-state-in-effect
      void authenticate();
    }
  }, [autoPrompt, authenticate]);

  if (isUnlocked) {
    return <>{children}</>;
  }

  if (fallbackComponent) {
    if (typeof fallbackComponent === "function") {
      return <>{fallbackComponent(authenticate)}</>;
    }
    return <>{fallbackComponent}</>;
  }

  return (
    <View style={[styles.container, containerStyle]}>
      <Text style={styles.lockIcon}>🔒</Text>
      <Text style={styles.title}>Protected Content</Text>
      <Text style={styles.subtitle}>{promptMessage}</Text>

      {errorMessage ? <Text style={styles.errorText}>{errorMessage}</Text> : null}

      <TouchableOpacity style={styles.unlockButton} onPress={authenticate} activeOpacity={0.8}>
        <Text style={styles.unlockButtonText}>Unlock with Biometrics</Text>
      </TouchableOpacity>
    </View>
  );
}

const styles = StyleSheet.create({
  container: {
    padding: 24,
    backgroundColor: "#F9FAFB",
    borderRadius: 12,
    borderWidth: 1,
    borderColor: "#E5E7EB",
    alignItems: "center",
    margin: 16,
  },
  lockIcon: {
    fontSize: 32,
    marginBottom: 8,
  },
  title: {
    fontSize: 18,
    fontWeight: "700",
    color: "#111827",
    marginBottom: 4,
  },
  subtitle: {
    fontSize: 14,
    color: "#6B7280",
    textAlign: "center",
    marginBottom: 16,
  },
  unlockButton: {
    backgroundColor: "#10B981",
    paddingVertical: 12,
    paddingHorizontal: 20,
    borderRadius: 8,
  },
  unlockButtonText: {
    color: "#FFFFFF",
    fontWeight: "600",
    fontSize: 15,
  },
  errorText: {
    color: "#EF4444",
    fontSize: 13,
    marginBottom: 12,
    textAlign: "center",
  },
});
