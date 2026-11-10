import 'package:flutter/material.dart';

/// Parses SQLite FTS5 highlight tags (<b>match</b>) and renders styled TextSpan highlights.
class LiliaHighlightSnippet extends StatelessWidget {
  final String snippet;
  final TextStyle? textStyle;
  final TextStyle? highlightStyle;
  final int? maxLines;
  final TextOverflow overflow;

  const LiliaHighlightSnippet({
    super.key,
    required this.snippet,
    this.textStyle,
    this.highlightStyle,
    this.maxLines,
    this.overflow = TextOverflow.ellipsis,
  });

  @override
  Widget build(BuildContext context) {
    final defaultStyle = textStyle ?? Theme.of(context).textTheme.bodyMedium ?? const TextStyle();
    final defaultHighlight = highlightStyle ??
        defaultStyle.copyWith(
          backgroundColor: const Color(0xFFFEF08A),
          color: const Color(0xFF854D0E),
          fontWeight: FontWeight.bold,
        );

    final spans = <TextSpan>[];
    final regex = RegExp(r'<b>(.*?)</b>');
    int lastIndex = 0;

    for (final match in regex.allMatches(snippet)) {
      if (match.start > lastIndex) {
        spans.add(
          TextSpan(
            text: snippet.substring(lastIndex, match.start),
            style: defaultStyle,
          ),
        );
      }
      spans.add(
        TextSpan(
          text: match.group(1),
          style: defaultHighlight,
        ),
      );
      lastIndex = match.end;
    }

    if (lastIndex < snippet.length) {
      spans.add(
        TextSpan(
          text: snippet.substring(lastIndex),
          style: defaultStyle,
        ),
      );
    }

    return RichText(
      maxLines: maxLines,
      overflow: overflow,
      text: TextSpan(children: spans),
    );
  }
}
