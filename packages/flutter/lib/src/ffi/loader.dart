import 'dart:ffi';
import 'dart:io';

/// Loads the LiliaDB native library: [customPath], else the `LILIA_FFI_PATH` environment
/// variable, else the library the platform's build of the plugin ships.
DynamicLibrary loadLiliaDynamicLibrary([String? customPath]) {
  final explicit = customPath ?? Platform.environment['LILIA_FFI_PATH'];
  if (explicit != null && explicit.isNotEmpty) {
    return DynamicLibrary.open(explicit);
  }
  if (Platform.isIOS || Platform.isMacOS) {
    // Linked statically into the plugin through the xcframework of its podspec.
    return DynamicLibrary.process();
  }
  if (Platform.isAndroid || Platform.isLinux) {
    return DynamicLibrary.open('liblilia_ffi.so');
  }
  if (Platform.isWindows) {
    return DynamicLibrary.open('lilia_ffi.dll');
  }
  throw UnsupportedError('Unsupported operating system for LiliaDB: ${Platform.operatingSystem}');
}
