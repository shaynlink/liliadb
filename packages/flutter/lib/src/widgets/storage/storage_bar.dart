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

/// The shape of a LiliaDB database: bytes of its sorted runs on disk and of the recent
/// writes still in memory, with a checkpoint button.
class LiliaStorageBar extends StatefulWidget {
  final bool showCheckpointButton;
  final double barHeight;
  final VoidCallback? onMaintenanceComplete;
  final LiliaDatabase? database;

  const LiliaStorageBar({
    super.key,
    this.showCheckpointButton = true,
    this.barHeight = 10,
    this.onMaintenanceComplete,
    this.database,
  });

  @override
  State<LiliaStorageBar> createState() => _LiliaStorageBarState();
}

class _LiliaStorageBarState extends State<LiliaStorageBar> {
  StorageStats? _stats;
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
    final stats = _db!.sync.maintenance.storageStats();
    setState(() {
      _stats = stats;
    });
  }

  Future<void> _handleCheckpoint() async {
    if (_db == null || _isOperating) return;
    setState(() => _isOperating = true);
    try {
      _db!.sync.maintenance.checkpoint();
      _refresh();
      widget.onMaintenanceComplete?.call();
    } finally {
      if (mounted) setState(() => _isOperating = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final disk = _stats?.diskBytes ?? 0;
    final memory = _stats?.memtableBytes ?? 0;
    final total = max(disk + memory, 1);
    final diskPct = (disk / total).clamp(0.01, 1.0);
    final memoryPct = (memory / total).clamp(0.0, 1.0);

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
                _formatBytes(disk + memory),
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
                    flex: (diskPct * 100).toInt().clamp(1, 100),
                    child: Container(color: const Color(0xFF3B82F6)),
                  ),
                  if (memory > 0)
                    Expanded(
                      flex: (memoryPct * 100).toInt().clamp(1, 100),
                      child: Container(color: const Color(0xFFF59E0B)),
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
              _buildLegendItem(const Color(0xFF3B82F6), 'On disk: ${_formatBytes(disk)}'),
              _buildLegendItem(const Color(0xFFF59E0B), 'In memory: ${_formatBytes(memory)}'),
            ],
          ),
          const SizedBox(height: 12),

          // Actions
          if (widget.showCheckpointButton)
            Row(
              children: [
                OutlinedButton(
                  onPressed: _isOperating ? null : _handleCheckpoint,
                  child: const Text('Checkpoint'),
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
