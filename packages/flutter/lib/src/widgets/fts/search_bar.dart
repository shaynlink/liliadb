import 'dart:async';
import 'package:flutter/material.dart';
import '../../database.dart';
import '../../scope.dart';

/// Full-text search bar connected directly to LiliaDB FTS5 engine.
///
/// Features built-in debouncing, clear button, and live search progress indicator.
class LiliaSearchBar extends StatefulWidget {
  final String collection;
  final ValueChanged<List<FtsHit>> onSearch;
  final int limit;
  final int debounceMs;
  final String hintText;
  final VoidCallback? onClear;
  final LiliaDatabase? database;

  const LiliaSearchBar({
    super.key,
    required this.collection,
    required this.onSearch,
    this.limit = 20,
    this.debounceMs = 250,
    this.hintText = 'Search...',
    this.onClear,
    this.database,
  });

  @override
  State<LiliaSearchBar> createState() => _LiliaSearchBarState();
}

class _LiliaSearchBarState extends State<LiliaSearchBar> {
  late final TextEditingController _controller;
  Timer? _debounceTimer;
  bool _isSearching = false;
  LiliaDatabase? _db;

  @override
  void initState() {
    super.initState();
    _controller = TextEditingController();
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    _db = widget.database ?? LiliaScope.of(context);
  }

  void _onChanged(String query) {
    _debounceTimer?.cancel();

    if (query.trim().isEmpty) {
      setState(() {
        _isSearching = false;
      });
      widget.onSearch([]);
      return;
    }

    setState(() {
      _isSearching = true;
    });

    _debounceTimer = Timer(Duration(milliseconds: widget.debounceMs), () {
      if (_db == null || !mounted) return;
      try {
        final hits = _db!.sync.fts.search(widget.collection, query, limit: widget.limit);
        widget.onSearch(hits);
      } finally {
        if (mounted) {
          setState(() {
            _isSearching = false;
          });
        }
      }
    });
  }

  void _handleClear() {
    _controller.clear();
    _debounceTimer?.cancel();
    setState(() {
      _isSearching = false;
    });
    widget.onClear?.call();
    widget.onSearch([]);
  }

  @override
  void dispose() {
    _debounceTimer?.cancel();
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Container(
      decoration: BoxDecoration(
        color: Theme.of(context).colorScheme.surfaceVariant.withOpacity(0.5),
        borderRadius: BorderRadius.circular(12),
      ),
      child: TextField(
        controller: _controller,
        onChanged: _onChanged,
        decoration: InputDecoration(
          hintText: widget.hintText,
          prefixIcon: const Icon(Icons.search, size: 20),
          suffixIcon: _isSearching
              ? const Padding(
                  padding: EdgeInsets.all(12.0),
                  child: SizedBox(
                    width: 16,
                    height: 16,
                    child: CircularProgressIndicator.adaptive(strokeWidth: 2),
                  ),
                )
              : _controller.text.isNotEmpty
                  ? IconButton(
                      icon: const Icon(Icons.clear, size: 18),
                      onPressed: _handleClear,
                    )
                  : null,
          border: InputBorder.none,
          contentPadding: const EdgeInsets.symmetric(horizontal: 14, vertical: 12),
        ),
      ),
    );
  }
}
