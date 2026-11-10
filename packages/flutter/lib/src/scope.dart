import 'package:flutter/widgets.dart';
import 'database.dart';

/// Top-level inherited scope providing access to [LiliaDatabase] across the Flutter widget tree.
class LiliaScope extends StatefulWidget {
  final LiliaDatabase database;
  final Widget child;
  final bool autoFlushOnBackground;

  const LiliaScope({
    super.key,
    required this.database,
    required this.child,
    this.autoFlushOnBackground = true,
  });

  /// Retrieves the [LiliaDatabase] from the nearest [LiliaScope] ancestor.
  static LiliaDatabase of(BuildContext context) {
    final scope = context.dependOnInheritedWidgetOfExactType<_LiliaInheritedScope>();
    if (scope == null) {
      // Fallback to default singleton if available
      final def = LiliaDatabase.getDefault();
      if (def != null) return def;

      throw FlutterError(
        'LiliaScope.of() called with a context that does not contain a LiliaScope widget.\n'
        'Make sure to wrap your application or screen in a LiliaScope:\n'
        '  LiliaScope(\n'
        '    database: db,\n'
        '    child: MyApp(),\n'
        '  )',
      );
    }
    return scope.database;
  }

  @override
  State<LiliaScope> createState() => _LiliaScopeState();
}

class _LiliaScopeState extends State<LiliaScope> with WidgetsBindingObserver {
  @override
  void initState() {
    super.initState();
    if (widget.autoFlushOnBackground) {
      WidgetsBinding.instance.addObserver(this);
    }
  }

  @override
  void dispose() {
    if (widget.autoFlushOnBackground) {
      WidgetsBinding.instance.removeObserver(this);
    }
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (widget.autoFlushOnBackground &&
        (state == AppLifecycleState.paused || state == AppLifecycleState.inactive)) {
      widget.database.prepareForBackground();
    }
  }

  @override
  Widget build(BuildContext context) {
    return _LiliaInheritedScope(
      database: widget.database,
      child: widget.child,
    );
  }
}

class _LiliaInheritedScope extends InheritedWidget {
  final LiliaDatabase database;

  const _LiliaInheritedScope({
    required this.database,
    required super.child,
  });

  @override
  bool updateShouldNotify(_LiliaInheritedScope oldWidget) {
    return database != oldWidget.database;
  }
}

/// Convenient syntax sugar extension on [BuildContext].
extension LiliaBuildContext on BuildContext {
  /// Shorthand to access the nearest [LiliaDatabase].
  LiliaDatabase get lilia => LiliaScope.of(this);
}
