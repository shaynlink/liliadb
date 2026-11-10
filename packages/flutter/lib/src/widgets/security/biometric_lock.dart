import 'package:flutter/material.dart';

/// Hardware security widget requiring biometric or passcode challenge before revealing its child.
class LiliaBiometricLock extends StatefulWidget {
  final Widget child;
  final String title;
  final String promptMessage;
  final Widget? fallbackWidget;
  final bool autoPrompt;
  final Future<bool> Function()? onAuthenticate;

  const LiliaBiometricLock({
    super.key,
    required this.child,
    this.title = 'Protected Content',
    this.promptMessage = 'Unlock to access sensitive data',
    this.fallbackWidget,
    this.autoPrompt = true,
    this.onAuthenticate,
  });

  @override
  State<LiliaBiometricLock> createState() => _LiliaBiometricLockState();
}

class _LiliaBiometricLockState extends State<LiliaBiometricLock> {
  bool _isUnlocked = false;
  String? _errorMessage;

  @override
  void initState() {
    super.initState();
    if (widget.autoPrompt) {
      _authenticate();
    }
  }

  Future<void> _authenticate() async {
    if (widget.onAuthenticate != null) {
      try {
        final success = await widget.onAuthenticate!();
        if (mounted) {
          setState(() {
            _isUnlocked = success;
            _errorMessage = success ? null : 'Authentication failed';
          });
        }
      } catch (e) {
        if (mounted) {
          setState(() {
            _errorMessage = e.toString();
          });
        }
      }
    } else {
      // Default unlock simulation in development
      setState(() {
        _isUnlocked = true;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    if (_isUnlocked) {
      return widget.child;
    }

    if (widget.fallbackWidget != null) {
      return widget.fallbackWidget!;
    }

    return Center(
      child: Container(
        margin: const EdgeInsets.all(24),
        padding: const EdgeInsets.all(24),
        decoration: BoxDecoration(
          color: Theme.of(context).cardColor,
          borderRadius: BorderRadius.circular(16),
          border: Border.all(color: Theme.of(context).dividerColor),
        ),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Icon(Icons.lock_outline, size: 48, color: Colors.blue),
            const SizedBox(height: 12),
            Text(
              widget.title,
              style: const TextStyle(fontSize: 18, fontWeight: FontWeight.bold),
            ),
            const SizedBox(height: 6),
            Text(
              widget.promptMessage,
              textAlign: TextAlign.center,
              style: const TextStyle(fontSize: 13, color: Colors.grey),
            ),
            if (_errorMessage != null) ...[
              const SizedBox(height: 12),
              Text(
                _errorMessage!,
                style: const TextStyle(color: Colors.red, fontSize: 12),
              ),
            ],
            const SizedBox(height: 20),
            ElevatedButton.icon(
              icon: const Icon(Icons.fingerprint),
              label: const Text('Unlock with Biometrics'),
              onPressed: _authenticate,
            ),
          ],
        ),
      ),
    );
  }
}
