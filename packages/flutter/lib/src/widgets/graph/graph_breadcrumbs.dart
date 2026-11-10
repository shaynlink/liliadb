import 'package:flutter/material.dart';

/// Breadcrumbs navigation trail for Graph trees and hierarchies in LiliaDB.
class LiliaGraphBreadcrumbs extends StatelessWidget {
  final List<String> path;
  final void Function(String nodeId, int index)? onNodeTapped;
  final String separator;
  final TextStyle? nodeStyle;
  final TextStyle? activeNodeStyle;
  final TextStyle? separatorStyle;

  const LiliaGraphBreadcrumbs({
    super.key,
    required this.path,
    this.onNodeTapped,
    this.separator = '›',
    this.nodeStyle,
    this.activeNodeStyle,
    this.separatorStyle,
  });

  @override
  Widget build(BuildContext context) {
    return SingleChildScrollView(
      scrollDirection: Axis.horizontal,
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          for (int i = 0; i < path.length; i++) ...[
            InkWell(
              onTap: onNodeTapped != null ? () => onNodeTapped!(path[i], i) : null,
              borderRadius: BorderRadius.circular(4),
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 2),
                child: Text(
                  path[i],
                  style: i == path.length - 1
                      ? (activeNodeStyle ??
                          const TextStyle(
                            fontWeight: FontWeight.bold,
                            color: Colors.blue,
                          ))
                      : (nodeStyle ??
                          const TextStyle(
                            color: Colors.grey,
                          )),
                ),
              ),
            ),
            if (i < path.length - 1)
              Padding(
                padding: const EdgeInsets.symmetric(horizontal: 4),
                child: Text(
                  separator,
                  style: separatorStyle ?? const TextStyle(color: Colors.grey),
                ),
              ),
          ],
        ],
      ),
    );
  }
}
