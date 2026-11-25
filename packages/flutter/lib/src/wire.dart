import 'dart:convert';
import 'dart:typed_data';

/// An error the engine reported: a stable [code] (`CONFLICT`, `CLOSED`, `INVALID_INPUT`...),
/// a [message], and whether the call may succeed when [retryable] is retried.
class LiliaException implements Exception {
  final String code;
  final String message;
  final bool retryable;
  final Object? details;
  final String? requestId;

  const LiliaException({
    required this.code,
    required this.message,
    required this.retryable,
    this.details,
    this.requestId,
  });

  factory LiliaException.fromJson(Map<String, dynamic> json) => LiliaException(
        code: json['code'] as String? ?? 'UNKNOWN',
        message: json['message'] as String? ?? '',
        retryable: json['retryable'] as bool? ?? false,
        details: json['details'],
        requestId: json['request_id'] as String?,
      );

  @override
  String toString() => 'LiliaException($code: $message)';
}

/// The result a JS frame holds: `[u32 JSON length][JSON][u32 segment count]
/// [(u32 offset, u32 length) per segment][segment bytes]`, all little-endian. The JSON is
/// `{"Ok": {"type": ..., "value": ...}}` or `{"Err": error}`; its byte strings (keys, values,
/// vectors) are segment numbers, which this replaces with their bytes.
///
/// Returns the `Ok` response (`type`, `value`); throws the `Err` as a [LiliaException].
Map<String, dynamic> readResponse(Uint8List frame) {
  final view = ByteData.sublistView(frame);
  final jsonLength = view.getUint32(0, Endian.little);
  final result = jsonDecode(utf8.decode(Uint8List.sublistView(frame, 4, 4 + jsonLength))) as Map<String, dynamic>;
  final error = result['Err'];
  if (error != null) throw LiliaException.fromJson(error as Map<String, dynamic>);
  final response = result['Ok'] as Map<String, dynamic>;

  final table = 8 + jsonLength;
  final bytes = table + view.getUint32(4 + jsonLength, Endian.little) * 8;
  Uint8List segment(Object? index) {
    final at = table + (index as int) * 8;
    final start = bytes + view.getUint32(at, Endian.little);
    return Uint8List.sublistView(frame, start, start + view.getUint32(at + 4, Endian.little));
  }

  void entry(Map<String, dynamic> kv) {
    kv['key'] = segment(kv['key']);
    kv['value'] = segment(kv['value']);
  }

  final value = response['value'];
  switch (response['type']) {
    case 'kv':
      if (value != null) entry(value as Map<String, dynamic>);
    case 'kv_page':
      for (final kv in value as List<dynamic>) {
        entry(kv as Map<String, dynamic>);
      }
    case 'vector':
      if (value != null) (value as Map<String, dynamic>)['data'] = segment(value['data']);
  }
  return response;
}

/// The value of [response] when it is of type [type]; a `PROTOCOL` [LiliaException] otherwise.
T expectValue<T>(Map<String, dynamic> response, String type) {
  if (response['type'] != type) {
    throw LiliaException(
      code: 'PROTOCOL',
      message: 'expected a $type response, got ${response['type']}',
      retryable: false,
    );
  }
  return response['value'] as T;
}

/// A version as results carry it: a decimal string where the engine keeps all 64 bits.
int readVersion(Object? version) => version is String ? int.parse(version) : version as int;
