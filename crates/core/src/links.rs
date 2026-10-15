//! Extraction of explicit URLs from untrusted text (video descriptions,
//! transcripts). Only `http`/`https` URLs that parse cleanly are returned; the
//! text is never interpreted beyond that.

use url::Url;

/// Returns the distinct http(s) URLs in `text`, in order of first appearance.
pub fn extract_urls(text: &str) -> Vec<Url> {
    let mut out: Vec<Url> = Vec::new();
    for token in text.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'')) {
        let start = match token.find("http://").or_else(|| token.find("https://")) {
            Some(i) => i,
            None => continue,
        };
        // Trailing punctuation usually belongs to the sentence, not the URL.
        let candidate =
            token[start..].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}']);
        if let Ok(url) = Url::parse(candidate)
            && matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && !out.contains(&url)
        {
            out.push(url);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_urls_and_strips_sentence_punctuation() {
        let urls = extract_urls(
            "Veja https://cheatsheetseries.owasp.org/cheatsheets/Secure_Coding_with_AI_Cheat_Sheet.html. \
             E também (https://www.augustogalego.com/books), ou https://www.augustogalego.com/books",
        );
        let urls: Vec<_> = urls.iter().map(Url::as_str).collect();
        assert_eq!(
            urls,
            [
                "https://cheatsheetseries.owasp.org/cheatsheets/Secure_Coding_with_AI_Cheat_Sheet.html",
                "https://www.augustogalego.com/books",
            ]
        );
    }

    #[test]
    fn ignores_other_schemes_and_garbage() {
        assert!(extract_urls("javascript:alert(1) ftp://x.org mailto:a@b.c http://").is_empty());
    }
}
