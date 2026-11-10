import React, { useMemo } from 'react';
import {
  Text,
  StyleSheet,
  type StyleProp,
  type TextStyle,
  type TextProps,
} from 'react-native';

export interface LiliaHighlightSnippetProps extends TextProps {
  snippet: string;
  highlightStyle?: StyleProp<TextStyle>;
  textStyle?: StyleProp<TextStyle>;
}

interface SnippetPart {
  text: string;
  isHighlight: boolean;
}

/**
 * Idiomatic text component parsing SQLite FTS5 snippet tags (<b>match</b>)
 * and applying customizable highlight styling to matched terms.
 */
export function LiliaHighlightSnippet({
  snippet,
  highlightStyle,
  textStyle,
  style,
  ...textProps
}: LiliaHighlightSnippetProps): React.ReactElement {
  const parts = useMemo<SnippetPart[]>(() => {
    if (!snippet) return [];

    const result: SnippetPart[] = [];
    const regex = /<b>(.*?)<\/b>/g;
    let lastIndex = 0;
    let match: RegExpExecArray | null;

    while ((match = regex.exec(snippet)) !== null) {
      if (match.index > lastIndex) {
        result.push({
          text: snippet.substring(lastIndex, match.index),
          isHighlight: false,
        });
      }
      result.push({
        text: match[1],
        isHighlight: true,
      });
      lastIndex = regex.lastIndex;
    }

    if (lastIndex < snippet.length) {
      result.push({
        text: snippet.substring(lastIndex),
        isHighlight: false,
      });
    }

    return result;
  }, [snippet]);

  return (
    <Text style={[styles.defaultText, textStyle, style]} {...textProps}>
      {parts.map((part, index) =>
        part.isHighlight ? (
          <Text key={index} style={[styles.defaultHighlight, highlightStyle]}>
            {part.text}
          </Text>
        ) : (
          part.text
        )
      )}
    </Text>
  );
}

const styles = StyleSheet.create({
  defaultText: {
    fontSize: 14,
    color: '#374151',
    lineHeight: 20,
  },
  defaultHighlight: {
    backgroundColor: '#FEF08A',
    color: '#854D0E',
    fontWeight: '700',
    borderRadius: 3,
  },
});
