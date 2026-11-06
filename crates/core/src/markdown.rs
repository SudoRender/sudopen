//! Markdown to a standalone HTML page for the preview.
//!
//! GitHub-flavored (tables, task lists, footnotes, strikethrough). Code
//! blocks are highlighted with the same syntaxes and themes as the text
//! view. Relative links and images are made absolute against the Markdown
//! file's folder, because the page itself is written to a cache folder.

use std::fmt::Write;
use std::sync::OnceLock;

use pulldown_cmark::{CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd, html};
use syntect::html::{ClassStyle, ClassedHTMLGenerator, css_for_theme_with_class_style};
use syntect::util::LinesWithEndings;

use crate::highlight::{pack, syntaxes, themes};

const CLASS: ClassStyle = ClassStyle::SpacedPrefixed { prefix: "hl-" };
/// Code blocks bigger than this are shown without highlighting.
const MAX_HL_BLOCK: usize = 512 * 1024;

/// Renders `src` as a full HTML document. `base_dir_url` is the file URL of
/// the Markdown file's folder, ending in `/`.
pub fn to_html(src: &str, title: &str, base_dir_url: &str) -> String {
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS
        | Options::ENABLE_GFM;
    let mut events = Vec::new();
    let mut code: Option<(String, String)> = None;
    let mut in_metadata = false;
    for ev in Parser::new_ext(src, opts) {
        match ev {
            Event::Start(Tag::MetadataBlock(_)) => in_metadata = true,
            Event::End(TagEnd::MetadataBlock(_)) => in_metadata = false,
            _ if in_metadata => {}
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split([',', ' ', '{']).next().unwrap_or("").to_string()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                code = Some((lang, String::new()));
            }
            Event::Text(t) if code.is_some() => {
                if let Some((_, buf)) = code.as_mut() {
                    buf.push_str(&t);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((lang, text)) = code.take() {
                    events.push(Event::Html(CowStr::from(highlight_code(&lang, &text))));
                }
            }
            Event::Start(Tag::Link { link_type, dest_url, title, id }) => {
                let dest_url = absolutize(&dest_url, base_dir_url).into();
                events.push(Event::Start(Tag::Link { link_type, dest_url, title, id }));
            }
            Event::Start(Tag::Image { link_type, dest_url, title, id }) => {
                let dest_url = absolutize(&dest_url, base_dir_url).into();
                events.push(Event::Start(Tag::Image { link_type, dest_url, title, id }));
            }
            e => events.push(e),
        }
    }
    let mut body = String::with_capacity(src.len() * 2);
    html::push_html(&mut body, events.into_iter());

    let mut page = String::with_capacity(body.len() + 8192);
    let _ = write!(
        page,
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title><style>{}{}</style></head><body><article>{}</article></body></html>",
        escape(title),
        PAGE_CSS,
        code_css(),
        body
    );
    page
}

fn highlight_code(lang: &str, text: &str) -> String {
    let ss = syntaxes();
    let syntax = (!lang.is_empty() && text.len() <= MAX_HL_BLOCK)
        .then(|| ss.find_syntax_by_token(lang))
        .flatten();
    if let Some(syntax) = syntax {
        let mut g = ClassedHTMLGenerator::new_with_class_style(syntax, ss, CLASS);
        let ok = LinesWithEndings::from(text).all(|l| g.parse_html_for_line_which_includes_newline(l).is_ok());
        if ok {
            return format!("<pre class=\"hl-code\"><code>{}</code></pre>\n", g.finalize());
        }
    }
    format!("<pre class=\"hl-code\"><code>{}</code></pre>\n", escape(text))
}

/// Theme CSS for code blocks, light and dark.
fn code_css() -> &'static str {
    static CSS: OnceLock<String> = OnceLock::new();
    CSS.get_or_init(|| {
        let t = themes();
        let light = css_for_theme_with_class_style(&t.light, CLASS).unwrap_or_default();
        let dark = css_for_theme_with_class_style(&t.dark, CLASS).unwrap_or_default();
        let bg = |c: Option<syntect::highlighting::Color>, fallback: u32| {
            format!("#{:08x}", c.map_or(fallback, pack))
        };
        format!(
            // Each theme only in its own scheme: a more specific light rule
            // would otherwise win in dark mode (dark-blue quotes on dark).
            "@media not (prefers-color-scheme: dark){{{light}\n:root{{--page:{};}}}}\n@media (prefers-color-scheme: dark){{{dark}\n:root{{--page:{};}}}}",
            bg(t.light.settings.background, 0xffffffff),
            bg(t.dark.settings.background, 0x282c34ff),
        )
    })
}

/// Makes a relative URL absolute against `base` (a folder URL ending in /).
/// Fragments, absolute URLs and `data:` stay as they are.
fn absolutize(dest: &str, base: &str) -> String {
    let has_scheme = dest
        .split_once(':')
        .is_some_and(|(s, _)| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)));
    if dest.is_empty() || dest.starts_with('#') || has_scheme {
        return dest.to_string();
    }
    if let Some(abs) = dest.strip_prefix('/') {
        return format!("file:///{abs}");
    }
    format!("{base}{dest}")
}

/// Percent-encodes a filesystem path for use in a file URL.
pub fn path_to_file_url(path: &str) -> String {
    let mut out = String::from("file://");
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

const PAGE_CSS: &str = r#"
:root { color-scheme: light dark; --fg:#1f2328; --muted:#59636e; --border:#d1d9e0; --soft:#f6f8fa; --link:#0969da; }
@media (prefers-color-scheme: dark) { :root { --fg:#dcdfe4; --muted:#9198a1; --border:#3d444d; --soft:#2f343e; --link:#61afef; } }
html { background: var(--page); }
body { margin: 0; color: var(--fg); background: var(--page); font: 15px/1.6 -apple-system, BlinkMacSystemFont, "Helvetica Neue", sans-serif; -webkit-font-smoothing: antialiased; }
article { max-width: 860px; margin: 0 auto; padding: 28px 40px 80px; overflow-wrap: break-word; }
h1, h2, h3, h4, h5, h6 { margin: 1.5em 0 0.6em; line-height: 1.25; font-weight: 600; }
h1 { font-size: 2em; padding-bottom: .3em; border-bottom: 1px solid var(--border); }
h2 { font-size: 1.5em; padding-bottom: .3em; border-bottom: 1px solid var(--border); }
h3 { font-size: 1.25em; } h4 { font-size: 1em; } h5 { font-size: .875em; } h6 { font-size: .85em; color: var(--muted); }
article > :first-child { margin-top: 0; }
p, ul, ol, blockquote, table, pre, dl { margin: 0 0 1em; }
a { color: var(--link); text-decoration: none; } a:hover { text-decoration: underline; }
ul, ol { padding-left: 2em; } li + li { margin-top: .25em; }
li > input[type=checkbox] { margin: 0 .4em 0 -1.3em; vertical-align: middle; }
ul:has(> li > input[type=checkbox]) { list-style: none; }
blockquote { padding: 0 1em; color: var(--muted); border-left: .25em solid var(--border); }
code, pre { font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 13px; }
:not(pre) > code { padding: .15em .4em; border-radius: 6px; background: var(--soft); }
pre { padding: 14px 16px; border-radius: 8px; overflow: auto; line-height: 1.45; background: var(--soft) !important; }
table { border-collapse: collapse; display: block; width: max-content; max-width: 100%; overflow: auto; }
th, td { padding: 6px 13px; border: 1px solid var(--border); }
th { font-weight: 600; } tr:nth-child(2n) td { background: var(--soft); }
img { max-width: 100%; }
hr { height: 2px; border: 0; background: var(--border); margin: 1.5em 0; }
.footnote-definition { font-size: .9em; color: var(--muted); }
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_gfm_and_code() {
        let html = to_html("# Hi\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```rust\nfn main() {}\n```\n", "t", "file:///x/");
        assert!(html.contains("<h1>Hi</h1>"));
        assert!(html.contains("<table>"));
        assert!(html.contains("hl-storage") || html.contains("hl-keyword"));
    }

    #[test]
    fn absolutizes_relative_urls_only() {
        assert_eq!(absolutize("img/a.png", "file:///docs/"), "file:///docs/img/a.png");
        assert_eq!(absolutize("#top", "file:///docs/"), "#top");
        assert_eq!(absolutize("https://x.dev", "file:///docs/"), "https://x.dev");
        assert_eq!(absolutize("/abs/p.png", "file:///docs/"), "file:///abs/p.png");
    }

    #[test]
    fn skips_front_matter() {
        let html = to_html("---\ntitle: x\n---\n\nbody\n", "t", "file:///");
        assert!(!html.contains("title: x"));
        assert!(html.contains("<p>body</p>"));
    }

    #[test]
    fn encodes_file_urls() {
        assert_eq!(path_to_file_url("/a b/c#d.md"), "file:///a%20b/c%23d.md");
    }
}
