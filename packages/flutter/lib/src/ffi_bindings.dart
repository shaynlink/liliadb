import 'dart:ffi';
import 'dart:io';

/// Resolves and loads the Lilia native dynamic library across platforms.
DynamicLibrary loadLiliaLibrary() {
  if (Platform.isMacOS) {
    // Check release framework or current dylib
    final dylibPath = 'release/apple/Lilia.xcframework/macos-arm64_x86_64/liblilia_ffi.a';
    if (File(dylibPath).existsSync()) {
      return DynamicLibrary.open(dylibPath);
    }
    // Fallback to process or local dylib
    try {
      return DynamicLibrary.open('liblilia_ffi.dylib');
    } catch (_) {
      return DynamicLibrary.process();
    }
  } else if (Platform.isIOS) {
    return DynamicLibrary.process();
  } else if (Platform.isAndroid) {
    return DynamicLibrary.open('liblilia_ffi.so');
  } else if (Platform.isWindows) {
    return DynamicLibrary.open('lilia_ffi.dll');
  } else if (Platform.isLinux) {
    return DynamicLibrary.open('liblilia_ffi.so');
  }
  throw UnsupportedError('Unsupported platform for LiliaDB: ${Platform.operatingSystem}');
}
