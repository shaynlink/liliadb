import 'dart:convert';
import 'dart:ffi';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';

import '../wire.dart';
import 'loader.dart';

/// An open database on the native side.
final class LiliaHandleOpaque extends Opaque {}

typedef _OpenC = Pointer<LiliaHandleOpaque> Function(Pointer<Utf8> options, Pointer<Pointer<Utf8>> outErr);
typedef _OpenDart = Pointer<LiliaHandleOpaque> Function(Pointer<Utf8> options, Pointer<Pointer<Utf8>> outErr);
typedef _ExecuteC = Pointer<Uint8> Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Uint8> request,
  Size len,
  Int32 encoding,
  Pointer<Size> outLen,
);
typedef _ExecuteDart = Pointer<Uint8> Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Uint8> request,
  int len,
  int encoding,
  Pointer<Size> outLen,
);
typedef _CloseC = Int32 Function(Pointer<LiliaHandleOpaque> handle, Uint64 timeoutMs, Pointer<Pointer<Utf8>> outErr);
typedef _CloseDart = int Function(Pointer<LiliaHandleOpaque> handle, int timeoutMs, Pointer<Pointer<Utf8>> outErr);
typedef _FreeBytesC = Void Function(Pointer<Uint8> bytes, Size len);
typedef _FreeBytesDart = void Function(Pointer<Uint8> bytes, int len);
typedef _FreeStringC = Void Function(Pointer<Utf8> message);
typedef _FreeStringDart = void Function(Pointer<Utf8> message);

/// Requests go to the C ABI as JSON (encoding 1); results come back as JS frames.
const int _jsonEncoding = 1;

/// The C ABI of `lilia-ffi`: open a database, run operations, close it.
class LiliaNativeBindings {
  LiliaNativeBindings._(DynamicLibrary library)
      : _open = library.lookupFunction<_OpenC, _OpenDart>('lilia_c_open'),
        _execute = library.lookupFunction<_ExecuteC, _ExecuteDart>('lilia_c_execute'),
        _close = library.lookupFunction<_CloseC, _CloseDart>('lilia_c_close'),
        _freeBytes = library.lookupFunction<_FreeBytesC, _FreeBytesDart>('lilia_c_free_bytes'),
        _freeString = library.lookupFunction<_FreeStringC, _FreeStringDart>('lilia_c_free_string');

  static LiliaNativeBindings? _instance;

  /// The bindings of the library [loadLiliaDynamicLibrary] finds.
  static LiliaNativeBindings get instance => _instance ??= LiliaNativeBindings._(loadLiliaDynamicLibrary());

  /// Uses the library at [path] from now on (tests, custom builds).
  static void useLibrary(String path) {
    _instance = LiliaNativeBindings._(loadLiliaDynamicLibrary(path));
  }

  final _OpenDart _open;
  final _ExecuteDart _execute;
  final _CloseDart _close;
  final _FreeBytesDart _freeBytes;
  final _FreeStringDart _freeString;

  /// Opens the database [options] describe (`path`, then any of `durability`, `profile`,
  /// `ledger`, `history_retention_ms`, `undo_depth`).
  Pointer<LiliaHandleOpaque> open(Map<String, Object?> options) {
    final json = jsonEncode(options).toNativeUtf8();
    final error = calloc<Pointer<Utf8>>();
    try {
      final handle = _open(json, error);
      if (handle == nullptr) throw _error(error.value);
      return handle;
    } finally {
      malloc.free(json);
      calloc.free(error);
    }
  }

  /// Runs [operation] (its JSON form); the JS frame of its result.
  Uint8List execute(Pointer<LiliaHandleOpaque> handle, Map<String, Object?> operation) {
    final request = utf8.encode(jsonEncode(operation));
    final requestPtr = malloc<Uint8>(request.length);
    final lengthPtr = calloc<Size>();
    try {
      requestPtr.asTypedList(request.length).setAll(0, request);
      final frame = _execute(handle, requestPtr, request.length, _jsonEncoding, lengthPtr);
      if (frame == nullptr) {
        throw const LiliaException(code: 'STORAGE', message: 'the result exceeds the frame limit', retryable: false);
      }
      final length = lengthPtr.value;
      try {
        return Uint8List.fromList(frame.asTypedList(length));
      } finally {
        _freeBytes(frame, length);
      }
    } finally {
      malloc.free(requestPtr);
      calloc.free(lengthPtr);
    }
  }

  /// Closes the database once its calls return, waiting at most [timeoutMs], and frees
  /// the handle.
  void close(Pointer<LiliaHandleOpaque> handle, int timeoutMs) {
    final error = calloc<Pointer<Utf8>>();
    try {
      if (_close(handle, timeoutMs, error) != 0) throw _error(error.value);
    } finally {
      calloc.free(error);
    }
  }

  /// The error the library reported as JSON, freed.
  LiliaException _error(Pointer<Utf8> message) {
    if (message == nullptr) {
      return const LiliaException(code: 'UNKNOWN', message: 'the native call failed', retryable: false);
    }
    try {
      return LiliaException.fromJson(jsonDecode(message.toDartString()) as Map<String, dynamic>);
    } finally {
      _freeString(message);
    }
  }
}
