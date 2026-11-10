import 'dart:math';
import 'package:flutter/material.dart';
import '../../database.dart';
import '../../scope.dart';

String _formatBytes(int bytes) {
  if (bytes <= 0) return '0 B';
  const k = 1024;
  const sizes = ['B', 'KB', 'MB', 'GB'];
  final i = (log(bytes) / log(k)).floor();
  return '${(bytes / pow(k, i)).toStringAsFixed(1)} ${sizes[i]}';
}

/// Visual storage breakdown bar and 1-tap maintenance component for LiliaDB.
class LiliaStorageBar extends StatefulWidget {
  final bool showVacuumButton;
  final bool showCheckpointButton;
  final double barHeight;
  final VoidCallback? onMaintenanceComplete;
  final LiliaDatabase? database;

  const LiliaStorageBar({
    super.key,
    this.showVacuumButton = true,
    this.showCheckpointButton = true,
    this.barHeight = 10,
    this.onMaintenanceComplete,
    this.database,
  });

  @override
  State<LiliaStorageBar> createState() => _LiliaStorageBarState();
}

class _LiliaStorageBarState extends State<LiliaStorageBar> {
  StorageMetrics? _metrics;
  bool _isOperating = false;
  LiliaDatabase? _db;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final db = widget.database ?? LiliaScope.of(context);
    if (_db != db) {
      _db = db;
      _refresh();
    }
  }

  void _refresh() {
    if (_db == null) return;
    final metrics = _db!.sync.maintenance.getFileSize();
    setState(() {
      _metrics = metrics;
    });
  }

  Future<void> _handleVacuum() async {
    if (_db == null || _isOperating) return;
    setState(() => _isOperating = true);
    try {
      _db!.sync.maintenance.vacuum();
      _refresh();
      widget.onMaintenanceComplete?.call();
    } finally {
      if (mounted) setState(() => _isOperating = false);
    }
  }

  Future<void> _handleCheckpoint() async {
    if (_db == null || _isOperating) return;
    setState(() => _isOperating = true);
    try {
      _db!.sync.maintenance.checkpoint('truncate');
      _refresh();
      widget.onMaintenanceComplete?.call();
    } finally {
      if (mounted) setState(() => _isOperating = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final metrics = _metrics ??
        const StorageMetrics(mainBytes: 0, walBytes: 0, shmBytes: 0, totalBytes: 1);

    final total = metrics.totalBytes > 0
        ? metrics.totalBytes
        : (metrics.mainBytes + metrics.walBytes + metrics.shmBytes).clamp(1, 1 << 62);

    final mainPct = (metrics.mainBytes / total).clamp(0.01, 1.0);
    final walPct = (metrics.walBytes / total).clamp(0.0, 1.0);
    final shmPct = (metrics.shmBytes / total).clamp(0.0, 1.0);

    return Container(
      padding: const EdgeInsets.all(16),
      decoration: BoxDecoration(
        color: Theme.of(context).cardColor,
        borderRadius: BorderRadius.circular(12),
        border: Border.all(color: Theme.of(context).dividerColor),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          Row(
            mainAxisAlignment: MainAxisAlignment.spaceBetween,
            children: [
              const Text(
                'LiliaDB Storage',
                style: TextStyle(fontWeight: FontWeight.bold, fontSize: 14),
              ),
              Text(
                _formatBytes(metrics.totalBytes),
                style: const TextStyle(fontWeight: FontWeight.bold, color: Colors.blue),
              ),
            ],
          ),
          const SizedBox(height: 12),

          // Segmented Progress Bar
          ClipRRect(
            borderRadius: BorderRadius.circular(widget.barHeight / 2),
            child: SizedBox(
              height: widget.barHeight,
              child: Row(
                children: [
                  Expanded(
                    flex: (mainPct * 100).toInt().clamp(1, 100),
                    child: Container(color: const Color(0xFF3B82F6)),
                  ),
                  if (metrics.walBytes > 0)
                    Expanded(
                      flex: (walPct * 100).toInt().clamp(1, 100),
                      child: Container(color: const Color(0xFFF59E0B)),
                    ),
                  if (metrics.shmBytes > 0)
                    Expanded(
                      flex: (shmPct * 100).toInt().clamp(1, 100),
                      child: Container(color: const Color(0xFF10B981)),
                    ),
                ],
              ),
            ),
          ),
          const SizedBox(height: 12),

          // Legend
          Row(
            mainAxisAlignment: MainAxisAlignment.spaceBetween,
            children: [
              _buildLegendItem(const Color(0xFF3B82F6), 'Data: ${_formatBytes(metrics.mainBytes)}'),
              _buildLegendItem(const Color(0xFFF59E0B), 'WAL: ${_formatBytes(metrics.walBytes)}'),
              _buildLegendItem(const Color(0xFF10B981), 'SHM: ${_formatBytes(metrics.shmBytes)}'),
            ],
          ),
          const SizedBox(height: 12),

          // Actions
          if (widget.showVacuumButton || widget.showCheckpointButton)
            Row(
              children: [
                if (widget.showCheckpointButton) ...[
                  OutlinedButton(
                    onPressed: _isOperating ? null : _handleCheckpoint,
                    child: const Text('Checkpoint WAL'),
                  ),
                  const SizedBox(width: 8),
                ],
                if (widget.showVacuumButton)
                  FilledButton.tonal(
                    onPressed: _isOperating ? null : _handleVacuum,
                    child: const Text('Vacuum DB'),
                  ),
                if (_isOperating) ...[
                  const SizedBox(width: 12),
                  const SizedBox(
                    width: 16,
                    height: 16,
                    child: CircularProgressIndicator.adaptive(strokeWidth: 2),
                  ),
                ],
              ],
            ),
        ],
      ),
    );
  }

  Widget _buildLegendItem(Color color, String label) {
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Container(
          width: 8,
          height: 8,
          decoration: BoxDecoration(color: color, shape: BoxShape.circle),
        ),
        const SizedBox(width: 4),
        Text(
          label,
          style: const TextStyle(fontSize: 11, color: Colors.grey),
        ),
      ],
    );
  }
}
