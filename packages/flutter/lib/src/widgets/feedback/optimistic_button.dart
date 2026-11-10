import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

/// Action button with optimistic loading state and native haptic feedback.
class LiliaOptimisticButton extends StatefulWidget {
  final Future<void> Function() onPressed;
  final Widget child;
  final Widget? loadingWidget;
  final bool haptic;
  final ButtonStyle? style;

  const LiliaOptimisticButton({
    super.key,
    required this.onPressed,
    required this.child,
    this.loadingWidget,
    this.haptic = true,
    this.style,
  });

  @override
  State<LiliaOptimisticButton> createState() => _LiliaOptimisticButtonState();
}

class _LiliaOptimisticButtonState extends State<LiliaOptimisticButton> {
  bool _isPending = false;

  Future<void> _handlePress() async {
    if (_isPending) return;

    if (widget.haptic) {
      HapticFeedback.lightImpact();
    }

    setState(() {
      _isPending = true;
    });

    try {
      await widget.onPressed();
    } finally {
      if (mounted) {
        setState(() {
          _isPending = false;
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    return ElevatedButton(
      style: widget.style,
      onPressed: _isPending ? null : _handlePress,
      child: _isPending
          ? (widget.loadingWidget ??
              const SizedBox(
                width: 18,
                height: 18,
                child: CircularProgressIndicator.adaptive(strokeWidth: 2),
              ))
          : widget.child,
    );
  }
}
