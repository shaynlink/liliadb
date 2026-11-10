import 'package:flutter/material.dart';

typedef LiliaErrorFallbackBuilder = Widget Function(
  BuildContext context,
  Object error,
  void Function() retry,
);

/// Error boundary catching Flutter rendering and storage failures in its subtree.
class LiliaErrorBoundary extends StatefulWidget {
  final Widget child;
  final LiliaErrorFallbackBuilder? fallbackBuilder;
  final void Function(Object error, StackTrace? stackTrace)? onError;

  const LiliaErrorBoundary({
    super.key,
    required this.child,
    this.fallbackBuilder,
    this.onError,
  });

  @override
  State<LiliaErrorBoundary> createState() => _LiliaErrorBoundaryState();
}

class _LiliaErrorBoundaryState extends State<LiliaErrorBoundary> {
  Object? _error;

  void retry() {
    setState(() {
      _error = null;
    });
  }

  void catchError(Object error, [StackTrace? stackTrace]) {
    widget.onError?.(error, stackTrace);
    setState(() {
      _error = error;
    });
  }

  @override
  Widget build(BuildContext context) {
    if (_error != null) {
      if (widget.fallbackBuilder != null) {
        return widget.fallbackBuilder!(context, _error!, retry);
      }

      return Container(
        margin: const EdgeInsets.all(16),
        padding: const EdgeInsets.all(20),
        decoration: BoxDecoration(
          color: const Color(0xFFFEF2F2),
          borderRadius: BorderRadius.circular(12),
          border: Border.all(color: const Color(0xFFF87171)),
        ),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Text(
              'Database / Render Error',
              style: TextStyle(
                color: Color(0xFF991B1B),
                fontWeight: FontWeight.bold,
                fontSize: 16,
              ),
            ),
            const SizedBox(height: 8),
            Text(
              _error.toString(),
              textAlign: TextAlign.center,
              style: const TextStyle(
                color: Color(0xFF7F1D1D),
                fontSize: 13,
              ),
            ),
            const SizedBox(height: 16),
            ElevatedButton(
              style: ElevatedButton.styleFrom(
                backgroundColor: const Color(0xFFDC2626),
                foregroundColor: Colors.white,
              ),
              onPressed: retry,
              child: const Text('Retry'),
            ),
          ],
        ),
      );
    }

    return widget.child;
  }
}
