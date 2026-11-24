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

const OPENING_TAG = '<b>';
const CLOSING_TAG = '</b>';

/**
 * Split the engine's trusted `<b>…</b>` markers without a backtracking regular
 * expression: search snippets are still external input once they reach the UI.
 */
export function splitSnippet(snippet: string): SnippetPart[] {
  const result: SnippetPart[] = [];
  let cursor = 0;

  while (cursor < snippet.length) {
    const openingIndex = snippet.indexOf(OPENING_TAG, cursor);
    if (openingIndex === -1) {
      result.push({ text: snippet.slice(cursor), isHighlight: false });
      break;
    }

    if (openingIndex > cursor) {
      result.push({ text: snippet.slice(cursor, openingIndex), isHighlight: false });
    }

    const contentStart = openingIndex + OPENING_TAG.length;
    const closingIndex = snippet.indexOf(CLOSING_TAG, contentStart);
    if (closingIndex === -1) {
      // Preserve malformed markup as text, just as a non-matching marker would.
      result.push({ text: snippet.slice(openingIndex), isHighlight: false });
      break;
    }

    result.push({
      text: snippet.slice(contentStart, closingIndex),
      isHighlight: true,
    });
    cursor = closingIndex + CLOSING_TAG.length;
  }

  return result;
}

/**
 * Idiomatic text component showing the snippet of a full-text hit, whose matches the engine
 * wraps in <b>match</b>, with customizable highlight styling for the matched terms.
 */
export function LiliaHighlightSnippet({
  snippet,
  highlightStyle,
  textStyle,
  style,
  ...textProps
}: LiliaHighlightSnippetProps): React.ReactElement {
  const parts = useMemo<SnippetPart[]>(() => splitSnippet(snippet), [snippet]);

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
