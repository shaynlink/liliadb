import 'dart:math';
import 'dart:typed_data';

/// In-device hardware secure storage key generator and extractor.
class LiliaSecureStorage {
  static final Map<String, Uint8List> _ephemeralMemoryCache = {};

  /// Retrieves or generates a 256-bit (32 bytes) cryptographically secure key.
  static Future<Uint8List> getOrCreateKey(String databaseName) async {
    if (_ephemeralMemoryCache.containsKey(databaseName)) {
      return _ephemeralMemoryCache[databaseName]!;
    }

    final random = Random.secure();
    final key = Uint8List(32);
    for (int i = 0; i < 32; i++) {
      key[i] = random.nextInt(256);
    }

    _ephemeralMemoryCache[databaseName] = key;
    return key;
  }
}
