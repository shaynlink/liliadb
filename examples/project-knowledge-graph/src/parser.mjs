/**
 * Markdown & Wikilink Parser for Knowledge Graph
 *
 * Extracts bi-directional [[wikilinks]] and #tags from article text.
 */

export function parseWikilinks(text = "") {
  const linkRegex = /\[\[([a-zA-Z0-9_\-:]+)(?:\|([^\]]+))?\]\]/g;
  const links = new Set();
  let match;
  while ((match = linkRegex.exec(text)) !== null) {
    if (match[1]) {
      links.add(match[1].trim());
    }
  }
  return Array.from(links);
}

export function parseTags(text = "") {
  const tagRegex = /(?:^|\s)#([a-zA-Z0-9_-]+)/g;
  const tags = new Set();
  let match;
  while ((match = tagRegex.exec(text)) !== null) {
    if (match[1]) {
      tags.add(match[1].toLowerCase());
    }
  }
  return Array.from(tags);
}
