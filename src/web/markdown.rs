//! Markdown -> sanitized HTML (model cards, comments).

use pulldown_cmark::{html, Options, Parser};

pub fn render(src: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_FOOTNOTES);
    let parser = Parser::new_ext(src, opts);
    let mut out = String::new();
    html::push_html(&mut out, parser);
    ammonia::Builder::default()
        .add_generic_attributes(["align"])
        .add_tag_attributes("img", ["width", "height"])
        .clean(&out)
        .to_string()
}
