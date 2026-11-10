import 'dart:ffi';
import 'dart:io';

/// Resolves and loads the Lilia native dynamic library across all platforms.
DynamicLibrary loadLiliaDynamicLibrary([String? customPath]) {
  if (customPath != null && customPath.isNotEmpty) {
    return DynamicLibrary.open(customPath);
  }

  if (Platform.isIOS) {
    return DynamicLibrary.process();
  } else if (Platform.isMacOS) {
    const candidates = [
      'release/apple/Lilia.xcframework/macos-arm64_x86_64/liblilia_ffi.a',
      'liblilia_ffi.dylib',
      'target/release/liblilia_ffi.dylib',
      'target/debug/liblilia_ffi.dylib',
    ];
    for (final path in candidates) {
      if (File(path).existsSync()) {
        try {
          return DynamicLibrary.open(path);
        } catch (_) {}
      }
    }
    try {
      return DynamicLibrary.open('liblilia_ffi.dylib');
    } catch (_) {
      return DynamicLibrary.process();
    }
  } else if (Platform.isAndroid) {
    return DynamicLibrary.open('liblilia_ffi.so');
  } else if (Platform.isWindows) {
    return DynamicLibrary.open('lilia_ffi.dll');
  } else if (Platform.isLinux) {
    return DynamicLibrary.open('liblilia_ffi.so');
  }

  throw UnsupportedError('Unsupported operating system for LiliaDB: ${Platform.operatingSystem}');
}
