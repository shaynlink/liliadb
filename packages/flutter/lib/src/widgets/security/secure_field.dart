import 'dart:async';
import 'package:flutter/material.dart';

/// Masked security field with one-tap reveal and auto-hide timer.
///
/// Prevents shoulder surfing for private keys, API secrets, and sensitive tokens.
class LiliaSecureField extends StatefulWidget {
  final String value;
  final String maskChar;
  final Duration autoHideTimeout;
  final TextStyle? style;
  final ValueChanged<bool>? onReveal;

  const LiliaSecureField({
    super.key,
    required this.value,
    this.maskChar = '•',
    this.autoHideTimeout = const Duration(seconds: 8),
    this.style,
    this.onReveal,
  });

  @override
  State<LiliaSecureField> createState() => _LiliaSecureFieldState();
}

class _LiliaSecureFieldState extends State<LiliaSecureField> {
  bool _isRevealed = false;
  Timer? _timer;

  void _toggleReveal() {
    _timer?.cancel();
    setState(() {
      _isRevealed = !_isRevealed;
      widget.onReveal?.call(_isRevealed);

      if (_isRevealed && widget.autoHideTimeout.inMilliseconds > 0) {
        _timer = Timer(widget.autoHideTimeout, () {
          if (mounted) {
            setState(() {
              _isRevealed = false;
              widget.onReveal?.call(false);
            });
          }
        });
      }
    });
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final displayText = _isRevealed
        ? widget.value
        : widget.maskChar * (widget.value.length.clamp(1, 24));

    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 8),
      decoration: BoxDecoration(
        color: Theme.of(context).colorScheme.surfaceVariant.withOpacity(0.5),
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: Theme.of(context).dividerColor),
      ),
      child: Row(
        children: [
          Expanded(
            child: Text(
              displayText,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: widget.style ??
                  const TextStyle(
                    fontFamily: 'monospace',
                    fontSize: 14,
                    letterSpacing: 1.2,
                  ),
            ),
          ),
          IconButton(
            icon: Icon(
              _isRevealed ? Icons.visibility_off : Icons.visibility,
              size: 20,
            ),
            onPressed: _toggleReveal,
            tooltip: _isRevealed ? 'Hide' : 'Reveal',
          ),
        ],
      ),
    );
  }
}
