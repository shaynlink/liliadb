//! Native CJK (Chinese, Japanese, Korean) micro-segmentation and tokenization.

#[cfg(feature = "cjk")]
use std::collections::HashSet;
#[cfg(feature = "cjk")]
use std::sync::OnceLock;

#[cfg(feature = "cjk")]
static JIEBA: OnceLock<jieba_rs::Jieba> = OnceLock::new();

#[cfg(feature = "cjk")]
fn get_jieba() -> &'static jieba_rs::Jieba {
    JIEBA.get_or_init(jieba_rs::Jieba::new)
}

/// Checks if a character is a CJK Unified Ideograph (Chinese characters / Japanese Kanji).
#[inline]
#[must_use]
pub fn is_cjk_ideograph(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'   // CJK Unified Ideographs
        | '\u{3400}'..='\u{4DBF}' // CJK Extension A
        | '\u{20000}'..='\u{2A6DF}' // CJK Extension B
        | '\u{F900}'..='\u{FAFF}' // CJK Compatibility Ideographs
        | '\u{2F800}'..='\u{2FA1F}' // CJK Compatibility Supplement
    )
}

/// Checks if a character is Japanese Hiragana.
#[inline]
#[must_use]
pub fn is_hiragana(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{309F}')
}

/// Checks if a character is Japanese Katakana.
#[inline]
#[must_use]
pub fn is_katakana(c: char) -> bool {
    matches!(c, '\u{30A0}'..='\u{30FF}' | '\u{31F0}'..='\u{31FF}')
}

/// Checks if a character is Korean Hangul (syllables or jamo).
#[inline]
#[must_use]
pub fn is_hangul(c: char) -> bool {
    matches!(c,
        '\u{AC00}'..='\u{D7AF}'   // Hangul Syllables
        | '\u{1100}'..='\u{11FF}' // Hangul Jamo
        | '\u{3130}'..='\u{318F}' // Hangul Compatibility Jamo
    )
}

/// Checks if a character belongs to any CJK script (Ideographs, Hiragana, Katakana, Hangul).
#[inline]
#[must_use]
pub fn is_cjk(c: char) -> bool {
    is_cjk_ideograph(c) || is_hiragana(c) || is_katakana(c) || is_hangul(c)
}

/// Quickly determines if a text contains any CJK characters.
#[inline]
#[must_use]
pub fn contains_cjk(text: &str) -> bool {
    text.chars().any(is_cjk)
}

/// Segments CJK text into a list of word tokens.
#[must_use]
pub fn cjk_segment(text: &str) -> Vec<String> {
    if !contains_cjk(text) {
        return text
            .split_whitespace()
            .map(std::string::ToString::to_string)
            .collect();
    }

    let mut tokens = Vec::new();
    let mut current_non_cjk = String::new();
    let mut current_cjk = String::new();

    for c in text.chars() {
        if is_cjk(c) {
            if !current_non_cjk.is_empty() {
                for word in current_non_cjk.split_whitespace() {
                    tokens.push(word.to_string());
                }
                current_non_cjk.clear();
            }
            current_cjk.push(c);
        } else if c.is_alphanumeric() {
            if !current_cjk.is_empty() {
                segment_cjk_chunk(&current_cjk, &mut tokens);
                current_cjk.clear();
            }
            current_non_cjk.push(c);
        } else {
            // Whitespace or punctuation
            if !current_cjk.is_empty() {
                segment_cjk_chunk(&current_cjk, &mut tokens);
                current_cjk.clear();
            }
            if !current_non_cjk.is_empty() {
                for word in current_non_cjk.split_whitespace() {
                    tokens.push(word.to_string());
                }
                current_non_cjk.clear();
            }
        }
    }

    if !current_cjk.is_empty() {
        segment_cjk_chunk(&current_cjk, &mut tokens);
    }
    if !current_non_cjk.is_empty() {
        for word in current_non_cjk.split_whitespace() {
            tokens.push(word.to_string());
        }
    }

    tokens
}

/// Internal segmentation of a contiguous block of CJK characters.
fn segment_cjk_chunk(chunk: &str, tokens: &mut Vec<String>) {
    // Check if this chunk is purely Katakana
    if chunk.chars().all(is_katakana) {
        if !chunk.is_empty() {
            tokens.push(chunk.to_string());
        }
        return;
    }

    // Check if this chunk contains Hangul
    if chunk.chars().any(is_hangul) {
        segment_korean_chunk(chunk, tokens);
        return;
    }

    // Japanese Hiragana particles boundary detection
    if chunk.chars().any(is_hiragana) {
        segment_japanese_chunk(chunk, tokens);
        return;
    }

    // Chinese / Kanji Ideographs
    #[cfg(feature = "cjk")]
    {
        let jieba = get_jieba();
        let words = jieba.cut_for_search(chunk, false);
        let mut seen = HashSet::new();
        for w in words {
            let trimmed = w.word.trim();
            if !trimmed.is_empty() && seen.insert(trimmed.to_string()) {
                tokens.push(trimmed.to_string());
            }
        }
    }

    #[cfg(not(feature = "cjk"))]
    {
        // Fallback: character unigrams and bigrams
        let chars: Vec<char> = chunk.chars().collect();
        for &c in &chars {
            tokens.push(c.to_string());
        }
        for w in chars.windows(2) {
            tokens.push(w.iter().collect());
        }
    }

    // Also add bigrams for guaranteed recall on sub-word matching
    let chars: Vec<char> = chunk.chars().collect();
    if chars.len() >= 2 {
        for w in chars.windows(2) {
            let bi: String = w.iter().collect();
            if !tokens.contains(&bi) {
                tokens.push(bi);
            }
        }
    }
}

/// Segments Japanese chunks containing Kanji and Hiragana particles.
fn segment_japanese_chunk(chunk: &str, tokens: &mut Vec<String>) {
    // Common Japanese grammatical particles acting as word boundaries
    const JP_PARTICLES: &[&str] = &[
        "から", "まで", "より", "など", "です", "ます", "した", "ない", "は", "が", "を", "に",
        "で", "と", "の", "へ", "も", "や",
    ];

    let mut current_stem = String::new();
    let mut i = 0;
    let chars: Vec<char> = chunk.chars().collect();

    while i < chars.len() {
        let remaining: String = chars[i..].iter().collect();

        // Check if remaining starts with a recognized particle
        let mut matched_particle = None;
        for &p in JP_PARTICLES {
            if remaining.starts_with(p) {
                matched_particle = Some(p);
                break;
            }
        }

        if let Some(p) = matched_particle {
            if !current_stem.is_empty() {
                emit_stem_tokens(&current_stem, tokens);
                current_stem.clear();
            }
            tokens.push(p.to_string());
            i += p.chars().count();
        } else {
            current_stem.push(chars[i]);
            i += 1;
        }
    }

    if !current_stem.is_empty() {
        emit_stem_tokens(&current_stem, tokens);
    }
}

fn emit_stem_tokens(stem: &str, tokens: &mut Vec<String>) {
    if stem.is_empty() {
        return;
    }
    tokens.push(stem.to_string());

    if stem.chars().any(is_cjk_ideograph) {
        #[cfg(feature = "cjk")]
        {
            let jieba = get_jieba();
            for w in jieba.cut_for_search(stem, false) {
                let trimmed = w.word.trim();
                if !trimmed.is_empty() && !tokens.contains(&trimmed.to_string()) {
                    tokens.push(trimmed.to_string());
                }
            }
        }

        let chars: Vec<char> = stem.chars().collect();
        if chars.len() >= 2 {
            for w in chars.windows(2) {
                let bi: String = w.iter().collect();
                if !tokens.contains(&bi) {
                    tokens.push(bi);
                }
            }
        }
    } else if stem.chars().any(is_katakana) {
        let mut katakana_word = String::new();
        for c in stem.chars() {
            if is_katakana(c) {
                katakana_word.push(c);
            } else if !katakana_word.is_empty() {
                if tokens.contains(&katakana_word) {
                    katakana_word.clear();
                } else {
                    tokens.push(std::mem::take(&mut katakana_word));
                }
            }
        }
        if !katakana_word.is_empty() && !tokens.contains(&katakana_word) {
            tokens.push(katakana_word);
        }
    }
}

/// Common Korean grammatical particles (josa) to strip for stem indexing.
const KO_PARTICLES: &[&str] = &[
    "에서", "으로", "에게", "한테", "은", "는", "이", "가", "을", "를", "에", "의", "로", "와",
    "과", "도", "만",
];

/// Segments Korean Hangul chunk by separating trailing grammatical particles (josa).
fn segment_korean_chunk(chunk: &str, tokens: &mut Vec<String>) {
    let mut stem = chunk.to_string();
    let mut matched_p = None;

    for &p in KO_PARTICLES {
        if stem.ends_with(p) && stem.len() > p.len() {
            matched_p = Some(p);
            stem.truncate(stem.len() - p.len());
            break;
        }
    }

    if !stem.is_empty() {
        tokens.push(stem.clone());
        let chars: Vec<char> = stem.chars().collect();
        if chars.len() >= 2 {
            for w in chars.windows(2) {
                tokens.push(w.iter().collect());
            }
        }
    }
    if let Some(p) = matched_p {
        tokens.push(p.to_string());
    }
}

/// Prepares text for full-text indexing. If the text contains CJK, words and bi-grams
/// are separated by spaces so a whitespace tokenizer indexes them individually.
#[must_use]
pub fn prepare_cjk_for_indexing(content: &str) -> String {
    if !contains_cjk(content) {
        return content.to_string();
    }

    let tokens = cjk_segment(content);
    tokens.join(" ")
}

/// Segments a CJK query into individual search keywords (without bigram inflation).
#[must_use]
pub fn cjk_query_words(text: &str) -> Vec<String> {
    if !contains_cjk(text) {
        return text
            .split_whitespace()
            .map(std::string::ToString::to_string)
            .collect();
    }

    let mut words = Vec::new();
    let mut current_non_cjk = String::new();
    let mut current_cjk = String::new();

    for c in text.chars() {
        if is_cjk(c) {
            if !current_non_cjk.is_empty() {
                for word in current_non_cjk.split_whitespace() {
                    words.push(word.to_string());
                }
                current_non_cjk.clear();
            }
            current_cjk.push(c);
        } else if c.is_alphanumeric() {
            if !current_cjk.is_empty() {
                segment_cjk_query_chunk(&current_cjk, &mut words);
                current_cjk.clear();
            }
            current_non_cjk.push(c);
        } else {
            if !current_cjk.is_empty() {
                segment_cjk_query_chunk(&current_cjk, &mut words);
                current_cjk.clear();
            }
            if !current_non_cjk.is_empty() {
                for word in current_non_cjk.split_whitespace() {
                    words.push(word.to_string());
                }
                current_non_cjk.clear();
            }
        }
    }

    if !current_cjk.is_empty() {
        segment_cjk_query_chunk(&current_cjk, &mut words);
    }
    if !current_non_cjk.is_empty() {
        for word in current_non_cjk.split_whitespace() {
            words.push(word.to_string());
        }
    }

    words
}

fn segment_cjk_query_chunk(chunk: &str, words: &mut Vec<String>) {
    if chunk.chars().all(is_katakana) {
        words.push(chunk.to_string());
        return;
    }

    if chunk.chars().any(is_hangul) {
        let mut stem = chunk.to_string();
        for &p in KO_PARTICLES {
            if stem.ends_with(p) && stem.len() > p.len() {
                stem.truncate(stem.len() - p.len());
                break;
            }
        }
        if !stem.is_empty() {
            words.push(stem);
        }
        return;
    }

    if chunk.chars().any(is_hiragana) {
        let mut sub_tokens = Vec::new();
        segment_japanese_chunk(chunk, &mut sub_tokens);
        for st in sub_tokens {
            if !st.is_empty() {
                words.push(st);
            }
        }
        return;
    }

    #[cfg(feature = "cjk")]
    {
        let jieba = get_jieba();
        let cut = jieba.cut(chunk, false);
        for w in cut {
            let t = w.word.trim();
            if !t.is_empty() {
                words.push(t.to_string());
            }
        }
    }

    #[cfg(not(feature = "cjk"))]
    {
        words.push(chunk.to_string());
    }
}

/// Prepares a query string for FTS5 search. If the query contains CJK characters,
/// it segments the query words into space-delimited tokens matching the indexed structure.
#[must_use]
pub fn prepare_cjk_for_search(query: &str) -> String {
    if !contains_cjk(query) {
        return query.to_string();
    }

    let tokens = cjk_query_words(query);
    if tokens.is_empty() {
        return query.to_string();
    }

    let mut clauses = Vec::new();
    for t in tokens {
        let clean = t.trim();
        if !clean.is_empty() {
            clauses.push(format!("\"{}\"", clean.replace('"', "\"\"")));
        }
    }

    if clauses.is_empty() {
        query.to_string()
    } else {
        clauses.join(" AND ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cjk_detection() {
        assert!(!contains_cjk("Hello world!"));
        assert!(!contains_cjk("Bonjour Gérard Smith"));
        assert!(contains_cjk("LiliaDB 数据库"));
        assert!(contains_cjk("日本語のテスト"));
        assert!(contains_cjk("한국어 전문 검색"));
    }

    #[test]
    fn test_chinese_segmentation() {
        let text = "LiliaDB 是一个高性能分布式多模型数据库";
        let tokens = cjk_segment(text);
        assert!(tokens.contains(&"高性能".to_string()));
        assert!(tokens.contains(&"分布式".to_string()));
        assert!(tokens.contains(&"数据库".to_string()));
    }

    #[test]
    fn test_japanese_segmentation() {
        let text = "東京特許許可局で日本語の全文検索をテストします";
        let tokens = cjk_segment(text);
        assert!(tokens.iter().any(|t| t == "東京" || t.contains("東京")));
        assert!(tokens.iter().any(|t| t == "特許" || t.contains("特許")));
        assert!(tokens
            .iter()
            .any(|t| t == "全文検索" || t.contains("全文検索")));
    }

    #[test]
    fn test_korean_segmentation() {
        let text = "데이터베이스에서 검색을 합니다";
        let tokens = cjk_segment(text);
        assert!(tokens.iter().any(|t| t == "데이터베이스"));
        assert!(tokens.iter().any(|t| t == "검색"));
    }

    #[test]
    fn test_prepare_indexing_and_search() {
        let indexed = prepare_cjk_for_indexing("高性能分布式数据库");
        assert!(indexed.contains("高性能"));
        assert!(indexed.contains("数据库"));

        let search_query = prepare_cjk_for_search("高性能数据库");
        assert!(search_query.contains("\"高性能\""));
        assert!(search_query.contains("\"数据库\""));
    }
}
