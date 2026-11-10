import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import '../../database.dart';
import '../../scope.dart';
import '../storage/storage_bar.dart';

/// Floating on-device DevTools inspector for LiliaDB active in [kDebugMode].
///
/// Features an embedded SQL console, storage telemetry, and KV store browser.
class LiliaDevInspector extends StatelessWidget {
  final bool enabled;
  final Alignment alignment;
  final LiliaDatabase? database;

  const LiliaDevInspector({
    super.key,
    this.enabled = kDebugMode,
    this.alignment = Alignment.bottomRight,
    this.database,
  });

  @override
  Widget build(BuildContext context) {
    if (!enabled) return const SizedBox.shrink();

    return Align(
      alignment: alignment,
      child: Padding(
        padding: const EdgeInsets.all(16.0),
        child: FloatingActionButton.extended(
          heroTag: 'lilia_dev_inspector_fab',
          backgroundColor: const Color(0xFF1E293B),
          foregroundColor: const Color(0xFF38BDF8),
          icon: const Icon(Icons.bolt, size: 20),
          label: const Text(
            'Lilia Inspector',
            style: TextStyle(fontWeight: FontWeight.bold, fontSize: 13),
          ),
          onPressed: () {
            final db = database ?? LiliaScope.of(context);
            showModalBottomSheet(
              context: context,
              isScrollControlled: true,
              backgroundColor: Theme.of(context).scaffoldBackgroundColor,
              shape: const RoundedRectangleBorder(
                borderRadius: BorderRadius.vertical(top: Radius.circular(20)),
              ),
              builder: (context) => _DevInspectorModal(database: db),
            );
          },
        ),
      ),
    );
  }
}

class _DevInspectorModal extends StatefulWidget {
  final LiliaDatabase database;

  const _DevInspectorModal({required this.database});

  @override
  State<_DevInspectorModal> createState() => _DevInspectorModalState();
}

class _DevInspectorModalState extends State<_DevInspectorModal> with SingleTickerProviderStateMixin {
  late final TabController _tabController;

  // SQL Console
  final TextEditingController _sqlController = TextEditingController(
    text: 'SELECT 1 as status, "Hello LiliaDB Flutter" as msg;',
  );
  SqlQueryResult<dynamic>? _sqlResult;
  String? _sqlError;
  bool _isExecutingSql = false;

  // KV Inspector
  final TextEditingController _kvNamespaceController = TextEditingController(text: 'app_settings');
  final TextEditingController _kvKeyController = TextEditingController(text: 'theme');
  final TextEditingController _kvValueController = TextEditingController();
  String? _kvStatus;

  @override
  void initState() {
    super.initState();
    _tabController = TabController(length: 3, vsync: this);
  }

  void _runSql() {
    setState(() {
      _isExecutingSql = true;
      _sqlError = null;
    });

    try {
      final res = widget.database.sync.sql.query(_sqlController.text);
      setState(() {
        _sqlResult = res;
      });
    } catch (e) {
      setState(() {
        _sqlError = e.toString();
      });
    } finally {
      setState(() {
        _isExecutingSql = false;
      });
    }
  }

  void _kvGet() {
    final val = widget.database.sync.kv.peek(
      _kvNamespaceController.text,
      _kvKeyController.text,
    );
    setState(() {
      _kvValueController.text = val ?? '<nil>';
      _kvStatus = 'Peek successful';
    });
  }

  void _kvSet() {
    widget.database.sync.kv.set(
      _kvNamespaceController.text,
      _kvKeyController.text,
      _kvValueController.text,
    );
    setState(() {
      _kvStatus = 'Value saved to KV';
    });
  }

  @override
  void dispose() {
    _tabController.dispose();
    _sqlController.dispose();
    _kvNamespaceController.dispose();
    _kvKeyController.dispose();
    _kvValueController.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      height: MediaQuery.of(context).size.height * 0.85,
      child: Column(
        children: [
          // Drag handle
          Container(
            margin: const EdgeInsets.symmetric(vertical: 10),
            width: 40,
            height: 4,
            decoration: BoxDecoration(
              color: Colors.grey.shade400,
              borderRadius: BorderRadius.circular(2),
            ),
          ),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16.0),
            child: Row(
              mainAxisAlignment: MainAxisAlignment.spaceBetween,
              children: [
                Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    const Text(
                      'LiliaDB Dev Inspector',
                      style: TextStyle(fontSize: 18, fontWeight: FontWeight.bold),
                    ),
                    Text(
                      widget.database.path,
                      style: const TextStyle(fontSize: 12, color: Colors.grey),
                    ),
                  ],
                ),
                IconButton(
                  icon: const Icon(Icons.close),
                  onPressed: () => Navigator.pop(context),
                ),
              ],
            ),
          ),
          TabBar(
            controller: _tabController,
            tabs: const [
              Tab(text: 'SQL Console'),
              Tab(text: 'Storage'),
              Tab(text: 'KV Store'),
            ],
          ),
          Expanded(
            child: TabBarView(
              controller: _tabController,
              children: [
                // 1. SQL Console Tab
                SingleChildScrollView(
                  padding: const EdgeInsets.all(16),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      const Text(
                        'SQL QUERY',
                        style: TextStyle(fontWeight: FontWeight.bold, fontSize: 12),
                      ),
                      const SizedBox(height: 6),
                      TextField(
                        controller: _sqlController,
                        maxLines: 3,
                        style: const TextStyle(fontFamily: 'monospace', fontSize: 13),
                        decoration: const InputDecoration(
                          border: OutlineInputBorder(),
                        ),
                      ),
                      const SizedBox(height: 10),
                      ElevatedButton.icon(
                        icon: const Icon(Icons.play_arrow),
                        label: const Text('Execute Query (⚡ FFI)'),
                        onPressed: _isExecutingSql ? null : _runSql,
                      ),
                      if (_sqlError != null) ...[
                        const SizedBox(height: 10),
                        Container(
                          padding: const EdgeInsets.all(10),
                          decoration: BoxDecoration(
                            color: Colors.red.shade50,
                            borderRadius: BorderRadius.circular(8),
                            border: Border.all(color: Colors.red.shade200),
                          ),
                          child: Text(
                            _sqlError!,
                            style: const TextStyle(color: Colors.red, fontFamily: 'monospace'),
                          ),
                        ),
                      ],
                      if (_sqlResult != null) ...[
                        const SizedBox(height: 14),
                        Text(
                          '${_sqlResult!.rows.length} rows returned (${_sqlResult!.rowsAffected} affected)',
                          style: const TextStyle(fontSize: 12, fontWeight: FontWeight.bold),
                        ),
                        const SizedBox(height: 8),
                        SingleChildScrollView(
                          scrollDirection: Axis.horizontal,
                          child: DataTable(
                            columns: _sqlResult!.columns
                                .map((c) => DataColumn(label: Text(c)))
                                .toList(),
                            rows: _sqlResult!.rows.map((r) {
                              final map = r is Map<String, dynamic> ? r : {'val': r.toString()};
                              return DataRow(
                                cells: _sqlResult!.columns
                                    .map((c) => DataCell(Text(map[c]?.toString() ?? 'NULL')))
                                    .toList(),
                              );
                            }).toList(),
                          ),
                        ),
                      ],
                    ],
                  ),
                ),

                // 2. Storage Tab
                SingleChildScrollView(
                  padding: const EdgeInsets.all(16),
                  child: LiliaStorageBar(database: widget.database),
                ),

                // 3. KV Store Tab
                SingleChildScrollView(
                  padding: const EdgeInsets.all(16),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      const Text('NAMESPACE', style: TextStyle(fontWeight: FontWeight.bold, fontSize: 12)),
                      const SizedBox(height: 6),
                      TextField(
                        controller: _kvNamespaceController,
                        decoration: const InputDecoration(border: OutlineInputBorder()),
                      ),
                      const SizedBox(height: 10),
                      const Text('KEY', style: TextStyle(fontWeight: FontWeight.bold, fontSize: 12)),
                      const SizedBox(height: 6),
                      TextField(
                        controller: _kvKeyController,
                        decoration: const InputDecoration(border: OutlineInputBorder()),
                      ),
                      const SizedBox(height: 10),
                      Row(
                        children: [
                          OutlinedButton(onPressed: _kvGet, child: const Text('Read Peek')),
                          const SizedBox(width: 8),
                          ElevatedButton(onPressed: _kvSet, child: const Text('Save Value')),
                        ],
                      ),
                      const SizedBox(height: 10),
                      const Text('VALUE', style: TextStyle(fontWeight: FontWeight.bold, fontSize: 12)),
                      const SizedBox(height: 6),
                      TextField(
                        controller: _kvValueController,
                        maxLines: 4,
                        style: const TextStyle(fontFamily: 'monospace'),
                        decoration: const InputDecoration(border: OutlineInputBorder()),
                      ),
                      if (_kvStatus != null) ...[
                        const SizedBox(height: 8),
                        Text(_kvStatus!, style: const TextStyle(color: Colors.green, fontStyle: FontStyle.italic)),
                      ],
                    ],
                  ),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}
