//! Untrusted HTML reduced to plain text and explicit links.
//!
//! Feed content is third-party HTML. It is never rendered or stored as HTML:
//! we keep readable text (for summaries) and absolute http(s) links (for
//! references). This is a tolerant tokenizer, not a validating parser; its
//! only job is to never let markup, scripts or styles through as text.

use url::Url;

/// Plain text, links (resolved against `base`), in document order.
pub fn to_text_and_links(html: &str, base: Option<&Url>) -> (String, Vec<Url>) {
    let mut text = String::with_capacity(html.len() / 2);
    let mut links: Vec<Url> = Vec::new();
    let mut rest = html;

    while let Some(lt) = rest.find('<') {
        push_text(&mut text, &rest[..lt]);
        rest = &rest[lt..];
        let Some(gt) = rest.find('>') else {
            // Unterminated tag: drop the remainder rather than leak markup.
            rest = "";
            break;
        };
        let tag = &rest[1..gt];
        rest = &rest[gt + 1..];

        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(name.as_str(), "script" | "style" | "noscript" | "template")
            && !tag.starts_with('/')
        {
            let close = format!("</{name}");
            rest = match rest.to_ascii_lowercase().find(&close) {
                Some(i) => rest[i..].find('>').map_or("", |j| &rest[i + j + 1..]),
                None => "",
            };
            continue;
        }
        if name == "a"
            && let Some(href) = attr(tag, "href")
            && let Some(url) = resolve(&decode_entities(&href), base)
            && !links.contains(&url)
        {
            links.push(url);
        }
        if matches!(
            name.as_str(),
            "p" | "br"
                | "div"
                | "li"
                | "ul"
                | "ol"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "pre"
                | "blockquote"
                | "tr"
        ) {
            text.push('\n');
        } else {
            text.push(' ');
        }
    }
    push_text(&mut text, rest);
    (tidy(&text), links)
}

fn push_text(out: &mut String, raw: &str) {
    out.push_str(&decode_entities(raw));
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower[from..].find(name) {
        let start = from + pos;
        from = start + name.len();
        // Must be a whole attribute name.
        if start > 0 && !lower.as_bytes()[start - 1].is_ascii_whitespace() {
            continue;
        }
        let after = tag[from..].trim_start();
        let Some(after) = after.strip_prefix('=') else {
            continue;
        };
        let after = after.trim_start();
        let value = match after.chars().next()? {
            q @ ('"' | '\'') => after[1..].split(q).next()?,
            _ => after
                .split(|c: char| c.is_whitespace() || c == '>')
                .next()?,
        };
        return Some(value.to_owned());
    }
    None
}

fn resolve(href: &str, base: Option<&Url>) -> Option<Url> {
    let url = match base {
        Some(b) => b.join(href).ok()?,
        None => Url::parse(href).ok()?,
    };
    matches!(url.scheme(), "http" | "https").then_some(url)
}

/// Decodes the entities that occur in practice: the five XML ones, `&nbsp;`,
/// and numeric references. Unknown named entities are left as written.
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        // Byte search: `;` is ASCII, so its index is always a char boundary,
        // whereas slicing the str at a fixed byte count may split a character.
        let window = &rest.as_bytes()[..rest.len().min(12)];
        let Some(semi) = window.iter().position(|&b| b == b';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            e if e.starts_with("#x") || e.starts_with("#X") => u32::from_str_radix(&e[2..], 16)
                .ok()
                .and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Collapses runs of spaces within lines and blank lines between paragraphs.
fn tidy(text: &str) -> String {
    let lines: Vec<String> = text
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    let mut out = String::new();
    let mut blank = true;
    for line in lines {
        if line.is_empty() {
            if !blank {
                out.push('\n');
            }
            blank = true;
            continue;
        }
        if !out.is_empty() && !blank {
            out.push('\n');
        }
        out.push_str(&line);
        blank = false;
    }
    out.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_markup_scripts_and_keeps_links() {
        let html = r#"<p>Leia o <a href="/cheatsheets/x.html?a=1&amp;b=2">guia</a> &amp; veja</p>
            <script>alert("<p>pwned</p>")</script><style>p{}</style>
            <p>Fim&nbsp;&#8212;&#x2713;</p><a href='javascript:alert(1)'>x</a>"#;
        let base = Url::parse("https://cheatsheetseries.owasp.org/index.html").unwrap();
        let (text, links) = to_text_and_links(html, Some(&base));
        assert_eq!(text, "Leia o guia & veja\nFim —✓\nx");
        assert_eq!(
            links.iter().map(Url::as_str).collect::<Vec<_>>(),
            ["https://cheatsheetseries.owasp.org/cheatsheets/x.html?a=1&b=2"]
        );
        assert!(!text.contains("pwned"));
    }

    #[test]
    fn unterminated_tags_do_not_leak() {
        let (text, _) = to_text_and_links("ok <img src=x onerror=alert(1)", None);
        assert_eq!(text, "ok");
    }

    #[test]
    fn ampersands_before_multibyte_text_do_not_panic() {
        // Found on a real feed: '&' followed by accented text, no ';' nearby.
        assert_eq!(decode_entities("P&D é ótimo; sim"), "P&D é ótimo; sim");
        assert_eq!(decode_entities("&ééééééé"), "&ééééééé");
    }

    #[test]
    fn unknown_entities_are_kept_verbatim() {
        assert_eq!(decode_entities("a &bogus; b &"), "a &bogus; b &");
    }
}
