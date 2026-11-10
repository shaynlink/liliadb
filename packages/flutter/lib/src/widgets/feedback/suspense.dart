import 'package:flutter/widgets.dart';

/// Zero-FOUC suspense gateway rendering a placeholder/skeleton while database data is hydrating.
class LiliaSuspense extends StatelessWidget {
  final Widget fallback;
  final bool isReady;
  final bool isLoading;
  final Widget child;

  const LiliaSuspense({
    super.key,
    required this.fallback,
    this.isReady = true,
    this.isLoading = false,
    required this.child,
  });

  @override
  Widget build(BuildContext context) {
    if (isLoading || !isReady) {
      return fallback;
    }
    return child;
  }
}
