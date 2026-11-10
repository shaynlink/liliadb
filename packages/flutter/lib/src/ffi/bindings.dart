import 'dart:convert';
import 'dart:ffi';
import 'dart:typed_data';
import 'package:ffi/ffi.dart';
import 'loader.dart';

// Opaque Handle
final class LiliaHandleOpaque extends Opaque {}

// C ABI Function Signatures
typedef _LiliaOpenC = Pointer<LiliaHandleOpaque> Function(
  Pointer<Utf8> path,
  Int32 profileCode,
  Pointer<Uint8> keyBytes,
  IntPtr keyLen,
  Pointer<Utf8> passphrase,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaOpenDart = Pointer<LiliaHandleOpaque> Function(
  Pointer<Utf8> path,
  int profileCode,
  Pointer<Uint8> keyBytes,
  int keyLen,
  Pointer<Utf8> passphrase,
  Pointer<Pointer<Utf8>> outErr,
);

typedef _LiliaFreeHandleC = Void Function(Pointer<LiliaHandleOpaque> handle);
typedef _LiliaFreeHandleDart = void Function(Pointer<LiliaHandleOpaque> handle);

typedef _LiliaFreeStringC = Void Function(Pointer<Utf8> str);
typedef _LiliaFreeStringDart = void Function(Pointer<Utf8> str);

typedef _LiliaFreeBytesC = Void Function(Pointer<Uint8> ptr, IntPtr len);
typedef _LiliaFreeBytesDart = void Function(Pointer<Uint8> ptr, int len);

typedef _LiliaCloseC = Int32 Function(
  Pointer<LiliaHandleOpaque> handle,
  Int32 timeoutMs,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaCloseDart = int Function(
  Pointer<LiliaHandleOpaque> handle,
  int timeoutMs,
  Pointer<Pointer<Utf8>> outErr,
);

typedef _LiliaPrepareForBackgroundC = Int32 Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaPrepareForBackgroundDart = int Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Pointer<Utf8>> outErr,
);

typedef _LiliaCheckpointC = Int32 Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> mode,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaCheckpointDart = int Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> mode,
  Pointer<Pointer<Utf8>> outErr,
);

typedef _LiliaVacuumC = Int32 Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaVacuumDart = int Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Pointer<Utf8>> outErr,
);

typedef _LiliaGetFileSizeC = Int32 Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Int64> outMain,
  Pointer<Int64> outWal,
  Pointer<Int64> outShm,
  Pointer<Int64> outTotal,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaGetFileSizeDart = int Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Int64> outMain,
  Pointer<Int64> outWal,
  Pointer<Int64> outShm,
  Pointer<Int64> outTotal,
  Pointer<Pointer<Utf8>> outErr,
);

typedef _LiliaBackupC = Int32 Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> destPath,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaBackupDart = int Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> destPath,
  Pointer<Pointer<Utf8>> outErr,
);

// KV
typedef _LiliaKvGetC = Int32 Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> namespace,
  Pointer<Uint8> keyBytes,
  IntPtr keyLen,
  Pointer<Pointer<Uint8>> outVal,
  Pointer<IntPtr> outValLen,
  Pointer<Int64> outVersion,
  Pointer<Int64> outExpiresAt,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaKvGetDart = int Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> namespace,
  Pointer<Uint8> keyBytes,
  int keyLen,
  Pointer<Pointer<Uint8>> outVal,
  Pointer<IntPtr> outValLen,
  Pointer<Int64> outVersion,
  Pointer<Int64> outExpiresAt,
  Pointer<Pointer<Utf8>> outErr,
);

typedef _LiliaKvSetC = Int32 Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> namespace,
  Pointer<Uint8> keyBytes,
  IntPtr keyLen,
  Pointer<Uint8> valBytes,
  IntPtr valLen,
  Int64 expiresAtMs,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaKvSetDart = int Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> namespace,
  Pointer<Uint8> keyBytes,
  int keyLen,
  Pointer<Uint8> valBytes,
  int valLen,
  int expiresAtMs,
  Pointer<Pointer<Utf8>> outErr,
);

typedef _LiliaKvDeleteC = Int32 Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> namespace,
  Pointer<Uint8> keyBytes,
  IntPtr keyLen,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaKvDeleteDart = int Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> namespace,
  Pointer<Uint8> keyBytes,
  int keyLen,
  Pointer<Pointer<Utf8>> outErr,
);

// JSON
typedef _LiliaJsonGetC = Pointer<Utf8> Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> space,
  Pointer<Utf8> id,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaJsonGetDart = Pointer<Utf8> Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> space,
  Pointer<Utf8> id,
  Pointer<Pointer<Utf8>> outErr,
);

typedef _LiliaJsonPutC = Int32 Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> space,
  Pointer<Utf8> id,
  Pointer<Utf8> jsonStr,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaJsonPutDart = int Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> space,
  Pointer<Utf8> id,
  Pointer<Utf8> jsonStr,
  Pointer<Pointer<Utf8>> outErr,
);

typedef _LiliaJsonDeleteC = Int32 Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> space,
  Pointer<Utf8> id,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaJsonDeleteDart = int Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> space,
  Pointer<Utf8> id,
  Pointer<Pointer<Utf8>> outErr,
);

typedef _LiliaJsonFindC = Pointer<Utf8> Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> space,
  Pointer<Utf8> queryJson,
  Pointer<Utf8> afterId,
  Int32 limit,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaJsonFindDart = Pointer<Utf8> Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> space,
  Pointer<Utf8> queryJson,
  Pointer<Utf8> afterId,
  int limit,
  Pointer<Pointer<Utf8>> outErr,
);

// GRAPH
typedef _LiliaGraphSetNodeC = Int32 Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> graph,
  Pointer<Utf8> id,
  Pointer<Utf8> kind,
  Pointer<Utf8> metadataJson,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaGraphSetNodeDart = int Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> graph,
  Pointer<Utf8> id,
  Pointer<Utf8> kind,
  Pointer<Utf8> metadataJson,
  Pointer<Pointer<Utf8>> outErr,
);

typedef _LiliaGraphGetNodeC = Pointer<Utf8> Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> graph,
  Pointer<Utf8> id,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaGraphGetNodeDart = Pointer<Utf8> Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> graph,
  Pointer<Utf8> id,
  Pointer<Pointer<Utf8>> outErr,
);

typedef _LiliaGraphDeleteNodeC = Int32 Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> graph,
  Pointer<Utf8> id,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaGraphDeleteNodeDart = int Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> graph,
  Pointer<Utf8> id,
  Pointer<Pointer<Utf8>> outErr,
);

typedef _LiliaGraphAddEdgeC = Int32 Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> graph,
  Pointer<Utf8> source,
  Pointer<Utf8> target,
  Pointer<Utf8> kind,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaGraphAddEdgeDart = int Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> graph,
  Pointer<Utf8> source,
  Pointer<Utf8> target,
  Pointer<Utf8> kind,
  Pointer<Pointer<Utf8>> outErr,
);

typedef _LiliaGraphRemoveEdgeC = Int32 Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> graph,
  Pointer<Utf8> source,
  Pointer<Utf8> target,
  Pointer<Utf8> kind,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaGraphRemoveEdgeDart = int Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> graph,
  Pointer<Utf8> source,
  Pointer<Utf8> target,
  Pointer<Utf8> kind,
  Pointer<Pointer<Utf8>> outErr,
);

typedef _LiliaGraphNeighborsC = Pointer<Utf8> Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> graph,
  Pointer<Utf8> id,
  Pointer<Utf8> direction,
  Pointer<Utf8> kindFilter,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaGraphNeighborsDart = Pointer<Utf8> Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> graph,
  Pointer<Utf8> id,
  Pointer<Utf8> direction,
  Pointer<Utf8> kindFilter,
  Pointer<Pointer<Utf8>> outErr,
);

typedef _LiliaGraphReachabilityC = Int32 Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> graph,
  Pointer<Utf8> source,
  Pointer<Utf8> target,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaGraphReachabilityDart = int Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> graph,
  Pointer<Utf8> source,
  Pointer<Utf8> target,
  Pointer<Pointer<Utf8>> outErr,
);

// FTS
typedef _LiliaFtsIndexC = Int32 Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> collection,
  Pointer<Utf8> docId,
  Pointer<Utf8> text,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaFtsIndexDart = int Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> collection,
  Pointer<Utf8> docId,
  Pointer<Utf8> text,
  Pointer<Pointer<Utf8>> outErr,
);

typedef _LiliaFtsDeleteC = Int32 Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> collection,
  Pointer<Utf8> docId,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaFtsDeleteDart = int Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> collection,
  Pointer<Utf8> docId,
  Pointer<Pointer<Utf8>> outErr,
);

typedef _LiliaFtsSearchC = Pointer<Utf8> Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> collection,
  Pointer<Utf8> query,
  Int32 limit,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaFtsSearchDart = Pointer<Utf8> Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> collection,
  Pointer<Utf8> query,
  int limit,
  Pointer<Pointer<Utf8>> outErr,
);

// SQL
typedef _LiliaSqlQueryC = Pointer<Utf8> Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> sql,
  Pointer<Utf8> paramsJson,
  Pointer<Pointer<Utf8>> outErr,
);
typedef _LiliaSqlQueryDart = Pointer<Utf8> Function(
  Pointer<LiliaHandleOpaque> handle,
  Pointer<Utf8> sql,
  Pointer<Utf8> paramsJson,
  Pointer<Pointer<Utf8>> outErr,
);

/// Singleton holder and wrapper for direct Dart FFI bindings to LiliaDB.
class LiliaNativeBindings {
  static LiliaNativeBindings? _instance;
  static LiliaNativeBindings get instance {
    _instance ??= LiliaNativeBindings._();
    return _instance!;
  }

  final DynamicLibrary? _dylib;

  late final _LiliaOpenDart? _open;
  late final _LiliaCloseDart? _close;
  late final _LiliaFreeHandleDart? _freeHandle;
  late final _LiliaFreeStringDart? _freeString;
  late final _LiliaFreeBytesDart? _freeBytes;
  late final _LiliaPrepareForBackgroundDart? _prepareForBackground;
  late final _LiliaCheckpointDart? _checkpoint;
  late final _LiliaVacuumDart? _vacuum;
  late final _LiliaGetFileSizeDart? _getFileSize;
  late final _LiliaBackupDart? _backup;

  late final _LiliaKvGetDart? _kvGet;
  late final _LiliaKvSetDart? _kvSet;
  late final _LiliaKvDeleteDart? _kvDelete;

  late final _LiliaJsonGetDart? _jsonGet;
  late final _LiliaJsonPutDart? _jsonPut;
  late final _LiliaJsonDeleteDart? _jsonDelete;
  late final _LiliaJsonFindDart? _jsonFind;

  late final _LiliaGraphSetNodeDart? _graphSetNode;
  late final _LiliaGraphGetNodeDart? _graphGetNode;
  late final _LiliaGraphDeleteNodeDart? _graphDeleteNode;
  late final _LiliaGraphAddEdgeDart? _graphAddEdge;
  late final _LiliaGraphRemoveEdgeDart? _graphRemoveEdge;
  late final _LiliaGraphNeighborsDart? _graphNeighbors;
  late final _LiliaGraphReachabilityDart? _graphReachability;

  late final _LiliaFtsIndexDart? _ftsIndex;
  late final _LiliaFtsDeleteDart? _ftsDelete;
  late final _LiliaFtsSearchDart? _ftsSearch;

  late final _LiliaSqlQueryDart? _sqlQuery;

  bool get isNativeAvailable => _dylib != null && _open != null;

  LiliaNativeBindings._() : _dylib = _tryLoad() {
    if (_dylib != null) {
      try {
        _open = _dylib!.lookupFunction<_LiliaOpenC, _LiliaOpenDart>('lilia_c_open');
        _close = _dylib!.lookupFunction<_LiliaCloseC, _LiliaCloseDart>('lilia_c_close');
        _freeHandle = _dylib!.lookupFunction<_LiliaFreeHandleC, _LiliaFreeHandleDart>('lilia_c_free_handle');
        _freeString = _dylib!.lookupFunction<_LiliaFreeStringC, _LiliaFreeStringDart>('lilia_c_free_string');
        _freeBytes = _dylib!.lookupFunction<_LiliaFreeBytesC, _LiliaFreeBytesDart>('lilia_c_free_bytes');
        _prepareForBackground = _dylib!.lookupFunction<_LiliaPrepareForBackgroundC, _LiliaPrepareForBackgroundDart>('lilia_c_prepare_for_background');
        _checkpoint = _dylib!.lookupFunction<_LiliaCheckpointC, _LiliaCheckpointDart>('lilia_c_checkpoint');
        _vacuum = _dylib!.lookupFunction<_LiliaVacuumC, _LiliaVacuumDart>('lilia_c_vacuum');
        _getFileSize = _dylib!.lookupFunction<_LiliaGetFileSizeC, _LiliaGetFileSizeDart>('lilia_c_get_file_size');
        _backup = _dylib!.lookupFunction<_LiliaBackupC, _LiliaBackupDart>('lilia_c_backup');

        _kvGet = _dylib!.lookupFunction<_LiliaKvGetC, _LiliaKvGetDart>('lilia_c_kv_get');
        _kvSet = _dylib!.lookupFunction<_LiliaKvSetC, _LiliaKvSetDart>('lilia_c_kv_set');
        _kvDelete = _dylib!.lookupFunction<_LiliaKvDeleteC, _LiliaKvDeleteDart>('lilia_c_kv_delete');

        _jsonGet = _dylib!.lookupFunction<_LiliaJsonGetC, _LiliaJsonGetDart>('lilia_c_json_get');
        _jsonPut = _dylib!.lookupFunction<_LiliaJsonPutC, _LiliaJsonPutDart>('lilia_c_json_put');
        _jsonDelete = _dylib!.lookupFunction<_LiliaJsonDeleteC, _LiliaJsonDeleteDart>('lilia_c_json_delete');
        _jsonFind = _dylib!.lookupFunction<_LiliaJsonFindC, _LiliaJsonFindDart>('lilia_c_json_find');

        _graphSetNode = _dylib!.lookupFunction<_LiliaGraphSetNodeC, _LiliaGraphSetNodeDart>('lilia_c_graph_set_node');
        _graphGetNode = _dylib!.lookupFunction<_LiliaGraphGetNodeC, _LiliaGraphGetNodeDart>('lilia_c_graph_get_node');
        _graphDeleteNode = _dylib!.lookupFunction<_LiliaGraphDeleteNodeC, _LiliaGraphDeleteNodeDart>('lilia_c_graph_delete_node');
        _graphAddEdge = _dylib!.lookupFunction<_LiliaGraphAddEdgeC, _LiliaGraphAddEdgeDart>('lilia_c_graph_add_edge');
        _graphRemoveEdge = _dylib!.lookupFunction<_LiliaGraphRemoveEdgeC, _LiliaGraphRemoveEdgeDart>('lilia_c_graph_remove_edge');
        _graphNeighbors = _dylib!.lookupFunction<_LiliaGraphNeighborsC, _LiliaGraphNeighborsDart>('lilia_c_graph_neighbors');
        _graphReachability = _dylib!.lookupFunction<_LiliaGraphReachabilityC, _LiliaGraphReachabilityDart>('lilia_c_graph_reachability');

        _ftsIndex = _dylib!.lookupFunction<_LiliaFtsIndexC, _LiliaFtsIndexDart>('lilia_c_fts_index');
        _ftsDelete = _dylib!.lookupFunction<_LiliaFtsDeleteC, _LiliaFtsDeleteDart>('lilia_c_fts_delete');
        _ftsSearch = _dylib!.lookupFunction<_LiliaFtsSearchC, _LiliaFtsSearchDart>('lilia_c_fts_search');

        _sqlQuery = _dylib!.lookupFunction<_LiliaSqlQueryC, _LiliaSqlQueryDart>('lilia_c_sql_query');
      } catch (_) {
        // Fallback for mock environments
      }
    }
  }

  static DynamicLibrary? _tryLoad() {
    try {
      return loadLiliaDynamicLibrary();
    } catch (_) {
      return null;
    }
  }

  Pointer<LiliaHandleOpaque> open({
    required String path,
    int profileCode = 0,
    Uint8List? keyBytes,
    String? passphrase,
  }) {
    if (!isNativeAvailable) return Pointer.fromAddress(1);

    final pathPtr = path.toNativeUtf8();
    final passPtr = passphrase != null ? passphrase.toNativeUtf8() : nullptr;
    Pointer<Uint8> keyPtr = nullptr;
    int keyLen = 0;

    if (keyBytes != null && keyBytes.length == 32) {
      keyPtr = calloc<Uint8>(32);
      for (int i = 0; i < 32; i++) {
        keyPtr[i] = keyBytes[i];
      }
      keyLen = 32;
    }

    final outErr = calloc<Pointer<Utf8>>();

    try {
      final handle = _open!(pathPtr, profileCode, keyPtr, keyLen, passPtr, outErr);
      if (outErr.value != nullptr) {
        final err = outErr.value.toDartString();
        _freeString!(outErr.value);
        throw StateError('[LiliaDB Native FFI] $err');
      }
      return handle;
    } finally {
      calloc.free(pathPtr);
      if (passPtr != nullptr) calloc.free(passPtr);
      if (keyPtr != nullptr) calloc.free(keyPtr);
      calloc.free(outErr);
    }
  }

  void close(Pointer<LiliaHandleOpaque> handle, {int timeoutMs = 5000}) {
    if (!isNativeAvailable || handle.address == 1) return;
    final outErr = calloc<Pointer<Utf8>>();
    try {
      _close!(handle, timeoutMs, outErr);
      if (outErr.value != nullptr) {
        _freeString!(outErr.value);
      }
      _freeHandle!(handle);
    } finally {
      calloc.free(outErr);
    }
  }

  void prepareForBackground(Pointer<LiliaHandleOpaque> handle) {
    if (!isNativeAvailable || handle.address == 1) return;
    final outErr = calloc<Pointer<Utf8>>();
    try {
      _prepareForBackground!(handle, outErr);
      if (outErr.value != nullptr) _freeString!(outErr.value);
    } finally {
      calloc.free(outErr);
    }
  }

  void checkpoint(Pointer<LiliaHandleOpaque> handle, [String mode = 'truncate']) {
    if (!isNativeAvailable || handle.address == 1) return;
    final modePtr = mode.toNativeUtf8();
    final outErr = calloc<Pointer<Utf8>>();
    try {
      _checkpoint!(handle, modePtr, outErr);
      if (outErr.value != nullptr) _freeString!(outErr.value);
    } finally {
      calloc.free(modePtr);
      calloc.free(outErr);
    }
  }

  void vacuum(Pointer<LiliaHandleOpaque> handle) {
    if (!isNativeAvailable || handle.address == 1) return;
    final outErr = calloc<Pointer<Utf8>>();
    try {
      _vacuum!(handle, outErr);
      if (outErr.value != nullptr) _freeString!(outErr.value);
    } finally {
      calloc.free(outErr);
    }
  }

  Map<String, int> getFileSize(Pointer<LiliaHandleOpaque> handle) {
    if (!isNativeAvailable || handle.address == 1) {
      return {'main': 0, 'wal': 0, 'shm': 0, 'total': 0};
    }

    final outMain = calloc<Int64>();
    final outWal = calloc<Int64>();
    final outShm = calloc<Int64>();
    final outTotal = calloc<Int64>();
    final outErr = calloc<Pointer<Utf8>>();

    try {
      _getFileSize!(handle, outMain, outWal, outShm, outTotal, outErr);
      return {
        'main': outMain.value,
        'wal': outWal.value,
        'shm': outShm.value,
        'total': outTotal.value,
      };
    } finally {
      calloc.free(outMain);
      calloc.free(outWal);
      calloc.free(outShm);
      calloc.free(outTotal);
      calloc.free(outErr);
    }
  }

  void backup(Pointer<LiliaHandleOpaque> handle, String destPath) {
    if (!isNativeAvailable || handle.address == 1) return;
    final destPtr = destPath.toNativeUtf8();
    final outErr = calloc<Pointer<Utf8>>();
    try {
      _backup!(handle, destPtr, outErr);
      if (outErr.value != nullptr) {
        final err = outErr.value.toDartString();
        _freeString!(outErr.value);
        throw StateError('[LiliaDB Native FFI Backup] $err');
      }
    } finally {
      calloc.free(destPtr);
      calloc.free(outErr);
    }
  }

  // --- KV ---
  Uint8List? kvGet(Pointer<LiliaHandleOpaque> handle, String namespace, Uint8List key) {
    if (!isNativeAvailable || handle.address == 1) return null;

    final nsPtr = namespace.toNativeUtf8();
    final keyPtr = calloc<Uint8>(key.length);
    for (int i = 0; i < key.length; i++) {
      keyPtr[i] = key[i];
    }

    final outVal = calloc<Pointer<Uint8>>();
    final outValLen = calloc<IntPtr>();
    final outVersion = calloc<Int64>();
    final outExpires = calloc<Int64>();
    final outErr = calloc<Pointer<Utf8>>();

    try {
      final res = _kvGet!(
        handle,
        nsPtr,
        keyPtr,
        key.length,
        outVal,
        outValLen,
        outVersion,
        outExpires,
        outErr,
      );

      if (res != 0 && outVal.value != nullptr && outValLen.value > 0) {
        final bytes = Uint8List.fromList(outVal.value.asTypedList(outValLen.value));
        _freeBytes!(outVal.value, outValLen.value);
        return bytes;
      }
      return null;
    } finally {
      calloc.free(nsPtr);
      calloc.free(keyPtr);
      calloc.free(outVal);
      calloc.free(outValLen);
      calloc.free(outVersion);
      calloc.free(outExpires);
      calloc.free(outErr);
    }
  }

  int kvSet(
    Pointer<LiliaHandleOpaque> handle,
    String namespace,
    Uint8List key,
    Uint8List value, {
    int expiresAtMs = 0,
  }) {
    if (!isNativeAvailable || handle.address == 1) return 1;

    final nsPtr = namespace.toNativeUtf8();
    final keyPtr = calloc<Uint8>(key.length);
    for (int i = 0; i < key.length; i++) {
      keyPtr[i] = key[i];
    }
    final valPtr = calloc<Uint8>(value.length);
    for (int i = 0; i < value.length; i++) {
      valPtr[i] = value[i];
    }

    final outErr = calloc<Pointer<Utf8>>();

    try {
      return _kvSet!(
        handle,
        nsPtr,
        keyPtr,
        key.length,
        valPtr,
        value.length,
        expiresAtMs,
        outErr,
      );
    } finally {
      calloc.free(nsPtr);
      calloc.free(keyPtr);
      calloc.free(valPtr);
      calloc.free(outErr);
    }
  }

  bool kvDelete(Pointer<LiliaHandleOpaque> handle, String namespace, Uint8List key) {
    if (!isNativeAvailable || handle.address == 1) return true;

    final nsPtr = namespace.toNativeUtf8();
    final keyPtr = calloc<Uint8>(key.length);
    for (int i = 0; i < key.length; i++) {
      keyPtr[i] = key[i];
    }
    final outErr = calloc<Pointer<Utf8>>();

    try {
      final res = _kvDelete!(handle, nsPtr, keyPtr, key.length, outErr);
      return res == 1;
    } finally {
      calloc.free(nsPtr);
      calloc.free(keyPtr);
      calloc.free(outErr);
    }
  }

  // --- JSON ---
  String? jsonGet(Pointer<LiliaHandleOpaque> handle, String space, String id) {
    if (!isNativeAvailable || handle.address == 1) return null;

    final spacePtr = space.toNativeUtf8();
    final idPtr = id.toNativeUtf8();
    final outErr = calloc<Pointer<Utf8>>();

    try {
      final strPtr = _jsonGet!(handle, spacePtr, idPtr, outErr);
      if (strPtr != nullptr) {
        final str = strPtr.toDartString();
        _freeString!(strPtr);
        return str;
      }
      return null;
    } finally {
      calloc.free(spacePtr);
      calloc.free(idPtr);
      calloc.free(outErr);
    }
  }

  int jsonPut(Pointer<LiliaHandleOpaque> handle, String space, String id, String jsonStr) {
    if (!isNativeAvailable || handle.address == 1) return 1;

    final spacePtr = space.toNativeUtf8();
    final idPtr = id.toNativeUtf8();
    final jsonPtr = jsonStr.toNativeUtf8();
    final outErr = calloc<Pointer<Utf8>>();

    try {
      return _jsonPut!(handle, spacePtr, idPtr, jsonPtr, outErr);
    } finally {
      calloc.free(spacePtr);
      calloc.free(idPtr);
      calloc.free(jsonPtr);
      calloc.free(outErr);
    }
  }

  bool jsonDelete(Pointer<LiliaHandleOpaque> handle, String space, String id) {
    if (!isNativeAvailable || handle.address == 1) return true;

    final spacePtr = space.toNativeUtf8();
    final idPtr = id.toNativeUtf8();
    final outErr = calloc<Pointer<Utf8>>();

    try {
      final res = _jsonDelete!(handle, spacePtr, idPtr, outErr);
      return res == 1;
    } finally {
      calloc.free(spacePtr);
      calloc.free(idPtr);
      calloc.free(outErr);
    }
  }

  String jsonFind(
    Pointer<LiliaHandleOpaque> handle,
    String space,
    String queryJson, {
    String? afterId,
    int limit = 50,
  }) {
    if (!isNativeAvailable || handle.address == 1) return '[]';

    final spacePtr = space.toNativeUtf8();
    final queryPtr = queryJson.toNativeUtf8();
    final afterPtr = afterId != null ? afterId.toNativeUtf8() : nullptr;
    final outErr = calloc<Pointer<Utf8>>();

    try {
      final strPtr = _jsonFind!(handle, spacePtr, queryPtr, afterPtr, limit, outErr);
      if (strPtr != nullptr) {
        final str = strPtr.toDartString();
        _freeString!(strPtr);
        return str;
      }
      return '[]';
    } finally {
      calloc.free(spacePtr);
      calloc.free(queryPtr);
      if (afterPtr != nullptr) calloc.free(afterPtr);
      calloc.free(outErr);
    }
  }

  // --- GRAPH ---
  void graphSetNode(Pointer<LiliaHandleOpaque> handle, String graph, String id, String kind, String metadataJson) {
    if (!isNativeAvailable || handle.address == 1) return;
    final graphPtr = graph.toNativeUtf8();
    final idPtr = id.toNativeUtf8();
    final kindPtr = kind.toNativeUtf8();
    final metaPtr = metadataJson.toNativeUtf8();
    final outErr = calloc<Pointer<Utf8>>();
    try {
      _graphSetNode!(handle, graphPtr, idPtr, kindPtr, metaPtr, outErr);
    } finally {
      calloc.free(graphPtr);
      calloc.free(idPtr);
      calloc.free(kindPtr);
      calloc.free(metaPtr);
      calloc.free(outErr);
    }
  }

  String? graphGetNode(Pointer<LiliaHandleOpaque> handle, String graph, String id) {
    if (!isNativeAvailable || handle.address == 1) return null;
    final graphPtr = graph.toNativeUtf8();
    final idPtr = id.toNativeUtf8();
    final outErr = calloc<Pointer<Utf8>>();
    try {
      final strPtr = _graphGetNode!(handle, graphPtr, idPtr, outErr);
      if (strPtr != nullptr) {
        final str = strPtr.toDartString();
        _freeString!(strPtr);
        return str;
      }
      return null;
    } finally {
      calloc.free(graphPtr);
      calloc.free(idPtr);
      calloc.free(outErr);
    }
  }

  bool graphDeleteNode(Pointer<LiliaHandleOpaque> handle, String graph, String id) {
    if (!isNativeAvailable || handle.address == 1) return true;
    final graphPtr = graph.toNativeUtf8();
    final idPtr = id.toNativeUtf8();
    final outErr = calloc<Pointer<Utf8>>();
    try {
      return _graphDeleteNode!(handle, graphPtr, idPtr, outErr) == 1;
    } finally {
      calloc.free(graphPtr);
      calloc.free(idPtr);
      calloc.free(outErr);
    }
  }

  void graphAddEdge(Pointer<LiliaHandleOpaque> handle, String graph, String source, String target, String kind) {
    if (!isNativeAvailable || handle.address == 1) return;
    final graphPtr = graph.toNativeUtf8();
    final srcPtr = source.toNativeUtf8();
    final tgtPtr = target.toNativeUtf8();
    final kindPtr = kind.toNativeUtf8();
    final outErr = calloc<Pointer<Utf8>>();
    try {
      _graphAddEdge!(handle, graphPtr, srcPtr, tgtPtr, kindPtr, outErr);
    } finally {
      calloc.free(graphPtr);
      calloc.free(srcPtr);
      calloc.free(tgtPtr);
      calloc.free(kindPtr);
      calloc.free(outErr);
    }
  }

  void graphRemoveEdge(Pointer<LiliaHandleOpaque> handle, String graph, String source, String target, String kind) {
    if (!isNativeAvailable || handle.address == 1) return;
    final graphPtr = graph.toNativeUtf8();
    final srcPtr = source.toNativeUtf8();
    final tgtPtr = target.toNativeUtf8();
    final kindPtr = kind.toNativeUtf8();
    final outErr = calloc<Pointer<Utf8>>();
    try {
      _graphRemoveEdge!(handle, graphPtr, srcPtr, tgtPtr, kindPtr, outErr);
    } finally {
      calloc.free(graphPtr);
      calloc.free(srcPtr);
      calloc.free(tgtPtr);
      calloc.free(kindPtr);
      calloc.free(outErr);
    }
  }

  List<String> graphNeighbors(
    Pointer<LiliaHandleOpaque> handle,
    String graph,
    String id, {
    String direction = 'out',
    String? kindFilter,
  }) {
    if (!isNativeAvailable || handle.address == 1) return [];
    final graphPtr = graph.toNativeUtf8();
    final idPtr = id.toNativeUtf8();
    final dirPtr = direction.toNativeUtf8();
    final kindPtr = kindFilter != null ? kindFilter.toNativeUtf8() : nullptr;
    final outErr = calloc<Pointer<Utf8>>();
    try {
      final strPtr = _graphNeighbors!(handle, graphPtr, idPtr, dirPtr, kindPtr, outErr);
      if (strPtr != nullptr) {
        final str = strPtr.toDartString();
        _freeString!(strPtr);
        final parsed = jsonDecode(str) as List<dynamic>;
        return parsed.map((e) => e.toString()).toList();
      }
      return [];
    } finally {
      calloc.free(graphPtr);
      calloc.free(idPtr);
      calloc.free(dirPtr);
      if (kindPtr != nullptr) calloc.free(kindPtr);
      calloc.free(outErr);
    }
  }

  bool graphReachability(Pointer<LiliaHandleOpaque> handle, String graph, String source, String target) {
    if (!isNativeAvailable || handle.address == 1) return false;
    final graphPtr = graph.toNativeUtf8();
    final srcPtr = source.toNativeUtf8();
    final tgtPtr = target.toNativeUtf8();
    final outErr = calloc<Pointer<Utf8>>();
    try {
      return _graphReachability!(handle, graphPtr, srcPtr, tgtPtr, outErr) == 1;
    } finally {
      calloc.free(graphPtr);
      calloc.free(srcPtr);
      calloc.free(tgtPtr);
      calloc.free(outErr);
    }
  }

  // --- FTS ---
  void ftsIndex(Pointer<LiliaHandleOpaque> handle, String collection, String docId, String text) {
    if (!isNativeAvailable || handle.address == 1) return;
    final colPtr = collection.toNativeUtf8();
    final docPtr = docId.toNativeUtf8();
    final txtPtr = text.toNativeUtf8();
    final outErr = calloc<Pointer<Utf8>>();
    try {
      _ftsIndex!(handle, colPtr, docPtr, txtPtr, outErr);
    } finally {
      calloc.free(colPtr);
      calloc.free(docPtr);
      calloc.free(txtPtr);
      calloc.free(outErr);
    }
  }

  bool ftsDelete(Pointer<LiliaHandleOpaque> handle, String collection, String docId) {
    if (!isNativeAvailable || handle.address == 1) return true;
    final colPtr = collection.toNativeUtf8();
    final docPtr = docId.toNativeUtf8();
    final outErr = calloc<Pointer<Utf8>>();
    try {
      return _ftsDelete!(handle, colPtr, docPtr, outErr) == 1;
    } finally {
      calloc.free(colPtr);
      calloc.free(docPtr);
      calloc.free(outErr);
    }
  }

  String ftsSearch(Pointer<LiliaHandleOpaque> handle, String collection, String query, {int limit = 20}) {
    if (!isNativeAvailable || handle.address == 1) return '[]';
    final colPtr = collection.toNativeUtf8();
    final qPtr = query.toNativeUtf8();
    final outErr = calloc<Pointer<Utf8>>();
    try {
      final strPtr = _ftsSearch!(handle, colPtr, qPtr, limit, outErr);
      if (strPtr != nullptr) {
        final str = strPtr.toDartString();
        _freeString!(strPtr);
        return str;
      }
      return '[]';
    } finally {
      calloc.free(colPtr);
      calloc.free(qPtr);
      calloc.free(outErr);
    }
  }

  // --- SQL ---
  String sqlQuery(Pointer<LiliaHandleOpaque> handle, String sql, [List<dynamic>? params]) {
    if (!isNativeAvailable || handle.address == 1) {
      return '{"columns":[],"rows":[],"rowsAffected":0}';
    }
    final sqlPtr = sql.toNativeUtf8();
    final paramsJson = jsonEncode(params ?? []);
    final pPtr = paramsJson.toNativeUtf8();
    final outErr = calloc<Pointer<Utf8>>();
    try {
      final strPtr = _sqlQuery!(handle, sqlPtr, pPtr, outErr);
      if (strPtr != nullptr) {
        final str = strPtr.toDartString();
        _freeString!(strPtr);
        return str;
      }
      return '{"columns":[],"rows":[],"rowsAffected":0}';
    } finally {
      calloc.free(sqlPtr);
      calloc.free(pPtr);
      calloc.free(outErr);
    }
  }
}
