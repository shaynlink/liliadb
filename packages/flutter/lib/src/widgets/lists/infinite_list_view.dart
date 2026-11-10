import 'package:flutter/material.dart';
import '../../database.dart';
import '../../scope.dart';

typedef LiliaJsonItemBuilder<T> = Widget Function(
  BuildContext context,
  JsonEntry<T> item,
  int index,
);

/// Cursor-based paginated infinite scrolling list connected directly to LiliaDB JSON space.
class LiliaInfiniteListView<T> extends StatefulWidget {
  final String space;
  final int pageSize;
  final Map<String, dynamic> filter;
  final LiliaJsonItemBuilder<T> itemBuilder;
  final Widget? loadingWidget;
  final Widget? emptyWidget;
  final Widget Function(Object error)? errorBuilder;
  final LiliaDatabase? database;

  const LiliaInfiniteListView({
    super.key,
    required this.space,
    this.pageSize = 20,
    this.filter = const {},
    required this.itemBuilder,
    this.loadingWidget,
    this.emptyWidget,
    this.errorBuilder,
    this.database,
  });

  @override
  State<LiliaInfiniteListView<T>> createState() => _LiliaInfiniteListViewState<T>();
}

class _LiliaInfiniteListViewState<T> extends State<LiliaInfiniteListView<T>> {
  final List<JsonEntry<T>> _items = [];
  final ScrollController _scrollController = ScrollController();

  bool _isLoading = true;
  bool _isLoadingMore = false;
  bool _hasMore = true;
  Object? _error;

  String? _lastId;
  LiliaDatabase? _db;
  void Function()? _unsubscribe;

  @override
  void initState() {
    super.initState();
    _scrollController.addListener(_onScroll);
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final db = widget.database ?? LiliaScope.of(context);
    if (_db != db) {
      _db = db;
      _initAndFetch();
    }
  }

  void _initAndFetch() {
    _unsubscribe?.call();
    if (_db == null) return;

    _fetchPage(isRefresh: true);

    _unsubscribe = _db!.subscribe('json:${widget.space}', () {
      if (mounted) _fetchPage(isRefresh: true);
    });
  }

  void _onScroll() {
    if (_scrollController.position.pixels >= _scrollController.position.maxScrollExtent - 200) {
      if (!_isLoadingMore && _hasMore && !_isLoading) {
        _fetchPage(isRefresh: false);
      }
    }
  }

  Future<void> _fetchPage({required bool isRefresh}) async {
    if (_db == null) return;

    if (isRefresh) {
      setState(() {
        _isLoading = true;
        _error = null;
        _lastId = null;
      });
    } else {
      setState(() {
        _isLoadingMore = true;
      });
    }

    try {
      final results = await _db!.json.find<T>(
        widget.space,
        widget.filter,
        afterId: isRefresh ? null : _lastId,
        limit: widget.pageSize,
      );

      setState(() {
        if (isRefresh) {
          _items.clear();
        }
        _items.addAll(results);

        if (results.isNotEmpty) {
          _lastId = results.last.id;
        }
        _hasMore = results.length >= widget.pageSize;
        _isLoading = false;
        _isLoadingMore = false;
      });
    } catch (e) {
      setState(() {
        _error = e;
        _isLoading = false;
        _isLoadingMore = false;
      });
    }
  }

  @override
  void dispose() {
    _scrollController.dispose();
    _unsubscribe?.call();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    if (_isLoading && widget.loadingWidget != null) {
      return widget.loadingWidget!;
    }

    if (_error != null) {
      if (widget.errorBuilder != null) return widget.errorBuilder!(_error!);
      return Center(child: Text('Error: $_error'));
    }

    if (_items.isEmpty && widget.emptyWidget != null) {
      return widget.emptyWidget!;
    }

    return RefreshIndicator(
      onRefresh: () => _fetchPage(isRefresh: true),
      child: ListView.builder(
        controller: _scrollController,
        itemCount: _items.length + (_isLoadingMore ? 1 : 0),
        itemBuilder: (context, index) {
          if (index == _items.length) {
            return const Padding(
              padding: EdgeInsets.symmetric(vertical: 16),
              child: Center(child: CircularProgressIndicator.adaptive()),
            );
          }
          return widget.itemBuilder(context, _items[index], index);
        },
      ),
    );
  }
}
