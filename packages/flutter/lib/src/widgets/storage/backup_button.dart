import 'package:flutter/material.dart';
import '../../database.dart';
import '../../scope.dart';

/// Instant hot backup button taking a snapshot of LiliaDB and sharing it.
class LiliaBackupShareButton extends StatefulWidget {
  final String title;
  final String? fileName;
  final ValueChanged<String>? onBackupComplete;
  final ValueChanged<Object>? onError;
  final Future<void> Function(String backupPath, String name)? onShare;
  final LiliaDatabase? database;

  const LiliaBackupShareButton({
    super.key,
    this.title = 'Export Backup',
    this.fileName,
    this.onBackupComplete,
    this.onError,
    this.onShare,
    this.database,
  });

  @override
  State<LiliaBackupShareButton> createState() => _LiliaBackupShareButtonState();
}

class _LiliaBackupShareButtonState extends State<LiliaBackupShareButton> {
  bool _isExporting = false;

  Future<void> _handleBackup(BuildContext context) async {
    final db = widget.database ?? LiliaScope.of(context);
    setState(() => _isExporting = true);

    try {
      final name = widget.fileName ?? 'lilia_backup_${DateTime.now().millisecondsSinceEpoch}.db';
      final backupPath = '${db.path}.backup_${DateTime.now().millisecondsSinceEpoch}.db';

      db.sync.maintenance.backup(backupPath);
      widget.onBackupComplete?.call(backupPath);

      if (widget.onShare != null) {
        await widget.onShare!(backupPath, name);
      } else {
        if (context.mounted) {
          ScaffoldMessenger.of(context).showSnackBar(
            SnackBar(content: Text('Backup created: $backupPath')),
          );
        }
      }
    } catch (e) {
      widget.onError?.call(e);
      if (context.mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text('Backup error: $e'), backgroundColor: Colors.red),
        );
      }
    } finally {
      if (mounted) setState(() => _isExporting = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    return ElevatedButton.icon(
      style: ElevatedButton.styleFrom(
        backgroundColor: const Color(0xFF059669),
        foregroundColor: Colors.white,
      ),
      icon: _isExporting
          ? const SizedBox(
              width: 16,
              height: 16,
              child: CircularProgressIndicator(strokeWidth: 2, color: Colors.white),
            )
          : const Icon(Icons.share, size: 18),
      label: Text(widget.title),
      onPressed: _isExporting ? null : () => _handleBackup(context),
    );
  }
}
