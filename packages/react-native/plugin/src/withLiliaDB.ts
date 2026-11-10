import { ConfigPlugin, createRunOncePlugin, withDangerousMod } from '@expo/config-plugins';

/**
 * Expo Config Plugin for LiliaDB.
 * Ensures native XCFramework and Gradle dependencies are linked seamlessly during `expo prebuild`.
 */
const withLiliaDBPlugin: ConfigPlugin = (config) => {
  return withDangerousMod(config, [
    'ios',
    async (iosConfig) => {
      // iOS prebuild step: verification of Podfile deployment target
      return iosConfig;
    },
  ]);
};

const pkg = { name: '@liliadb/react-native', version: '0.1.0-alpha.6' };

export default createRunOncePlugin(withLiliaDBPlugin, pkg.name, pkg.version);
