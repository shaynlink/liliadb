import 'package:flutter/material.dart';
import '../../database.dart';
import '../../scope.dart';

typedef LiliaGraphNodeBuilder = Widget Function(
  BuildContext context,
  String nodeId,
  bool isCurrent,
);

/// Interactive visual graph explorer for traversing nodes, relationships, and neighbors.
class LiliaGraphExplorer extends StatefulWidget {
  final String graph;
  final String rootNode;
  final String direction;
  final String? kindFilter;
  final ValueChanged<String>? onNodeSelected;
  final LiliaGraphNodeBuilder? nodeBuilder;
  final LiliaDatabase? database;

  const LiliaGraphExplorer({
    super.key,
    required this.graph,
    required this.rootNode,
    this.direction = 'out',
    this.kindFilter,
    this.onNodeSelected,
    this.nodeBuilder,
    this.database,
  });

  @override
  State<LiliaGraphExplorer> createState() => _LiliaGraphExplorerState();
}

class _LiliaGraphExplorerState extends State<LiliaGraphExplorer> {
  late String _currentNode;
  final List<String> _history = [];
  List<String> _neighbors = [];
  LiliaDatabase? _db;
  void Function()? _unsubscribe;

  @override
  void initState() {
    super.initState();
    _currentNode = widget.rootNode;
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final db = widget.database ?? LiliaScope.of(context);
    if (_db != db) {
      _db = db;
      _loadNeighbors();
      _subscribe();
    }
  }

  void _subscribe() {
    _unsubscribe?.call();
    if (_db == null) return;
    _unsubscribe = _db!.subscribe('graph:${widget.graph}', () {
      if (mounted) _loadNeighbors();
    });
  }

  void _loadNeighbors() {
    if (_db == null) return;
    final neighbors = _db!.sync.graph.neighbors(
      widget.graph,
      _currentNode,
      direction: widget.direction,
      kindFilter: widget.kindFilter,
    );
    setState(() {
      _neighbors = neighbors;
    });
  }

  void _selectNeighbor(String neighbor) {
    _history.add(_currentNode);
    setState(() {
      _currentNode = neighbor;
    });
    _loadNeighbors();
    widget.onNodeSelected?.call(neighbor);
  }

  void _goBack() {
    if (_history.isEmpty) return;
    final prev = _history.removeLast();
    setState(() {
      _currentNode = prev;
    });
    _loadNeighbors();
    widget.onNodeSelected?.call(prev);
  }

  void _reset() {
    _history.clear();
    setState(() {
      _currentNode = widget.rootNode;
    });
    _loadNeighbors();
    widget.onNodeSelected?.call(widget.rootNode);
  }

  @override
  void dispose() {
    _unsubscribe?.call();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
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
          // Navigation controls
          Row(
            mainAxisAlignment: MainAxisAlignment.spaceBetween,
            children: [
              if (_history.isNotEmpty)
                TextButton.icon(
                  icon: const Icon(Icons.arrow_back, size: 16),
                  label: const Text('Back'),
                  onPressed: _goBack,
                )
              else
                const SizedBox.shrink(),
              if (_currentNode != widget.rootNode)
                TextButton(
                  onPressed: _reset,
                  child: const Text('Reset Root'),
                ),
            ],
          ),
          const SizedBox(height: 8),

          // Current node display
          if (widget.nodeBuilder != null)
            widget.nodeBuilder!(context, _currentNode, true)
          else
            Container(
              width: double.infinity,
              padding: const EdgeInsets.all(16),
              decoration: BoxDecoration(
                color: const Color(0xFFEFF6FF),
                borderRadius: BorderRadius.circular(10),
                border: Border.all(color: const Color(0xFFBFDBFE)),
              ),
              child: Column(
                children: [
                  const Text(
                    'CURRENT NODE',
                    style: TextStyle(
                      fontSize: 11,
                      fontWeight: FontWeight.bold,
                      color: Color(0xFF3B82F6),
                    ),
                  ),
                  const SizedBox(height: 4),
                  Text(
                    _currentNode,
                    style: const TextStyle(
                      fontSize: 18,
                      fontWeight: FontWeight.bold,
                      color: Color(0xFF1E40AF),
                    ),
                  ),
                ],
              ),
            ),
          const SizedBox(height: 16),

          // Neighbors section
          Text(
            'NEIGHBORS (${_neighbors.length})',
            style: const TextStyle(
              fontSize: 12,
              fontWeight: FontWeight.bold,
              color: Colors.grey,
            ),
          ),
          const SizedBox(height: 8),

          if (_neighbors.isEmpty)
            const Padding(
              padding: EdgeInsets.symmetric(vertical: 12),
              child: Text(
                'No connected neighbors',
                style: TextStyle(color: Colors.grey, fontStyle: FontStyle.italic),
              ),
            )
          else
            SizedBox(
              height: 72,
              child: ListView.separated(
                scrollDirection: Axis.horizontal,
                itemCount: _neighbors.length,
                separatorBuilder: (context, _) => const SizedBox(width: 8),
                itemBuilder: (context, index) {
                  final neighbor = _neighbors[index];
                  if (widget.nodeBuilder != null) {
                    return GestureDetector(
                      onTap: () => _selectNeighbor(neighbor),
                      child: widget.nodeBuilder!(context, neighbor, false),
                    );
                  }
                  return InkWell(
                    onTap: () => _selectNeighbor(neighbor),
                    borderRadius: BorderRadius.circular(8),
                    child: Container(
                      padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
                      decoration: BoxDecoration(
                        color: Theme.of(context).colorScheme.surfaceVariant.withOpacity(0.5),
                        borderRadius: BorderRadius.circular(8),
                        border: Border.all(color: Theme.of(context).dividerColor),
                      ),
                      child: Column(
                        mainAxisAlignment: MainAxisAlignment.center,
                        children: [
                          Text(
                            neighbor,
                            style: const TextStyle(fontWeight: FontWeight.w600),
                          ),
                          const SizedBox(height: 2),
                          const Text(
                            'Explore →',
                            style: TextStyle(fontSize: 11, color: Colors.blue),
                          ),
                        ],
                      ),
                    ),
                  );
                },
              ),
            ),
        ],
      ),
    );
  }
}
