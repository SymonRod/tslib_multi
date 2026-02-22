//! BBCode parsing and rendering

use regex::Regex;

/// BBCode parser
pub struct BBCodeParser;

/// BBCode renderer for different output formats
pub trait BBCodeRenderer {
    /// Render bold text
    fn bold(&self, content: &str) -> String;
    /// Render italic text
    fn italic(&self, content: &str) -> String;
    /// Render underlined text
    fn underline(&self, content: &str) -> String;
    /// Render strikethrough text
    fn strike(&self, content: &str) -> String;
    /// Render colored text
    fn color(&self, color: &str, content: &str) -> String;
    /// Render sized text
    fn size(&self, size: &str, content: &str) -> String;
    /// Render a URL
    fn url(&self, url: &str, text: &str) -> String;
    /// Render an image
    fn image(&self, url: &str) -> String;
}

/// HTML renderer
pub struct HtmlRenderer;

impl BBCodeRenderer for HtmlRenderer {
    fn bold(&self, content: &str) -> String {
        format!("<strong>{}</strong>", content)
    }

    fn italic(&self, content: &str) -> String {
        format!("<em>{}</em>", content)
    }

    fn underline(&self, content: &str) -> String {
        format!("<u>{}</u>", content)
    }

    fn strike(&self, content: &str) -> String {
        format!("<s>{}</s>", content)
    }

    fn color(&self, color: &str, content: &str) -> String {
        format!("<span style=\"color: {}\">{}</span>", color, content)
    }

    fn size(&self, size: &str, content: &str) -> String {
        format!("<span style=\"font-size: {}px\">{}</span>", size, content)
    }

    fn url(&self, url: &str, text: &str) -> String {
        format!("<a href=\"{}\">{}</a>", url, text)
    }

    fn image(&self, url: &str) -> String {
        format!("<img src=\"{}\" />", url)
    }
}

/// Plain text renderer (strips BBCode)
pub struct PlainTextRenderer;

impl BBCodeRenderer for PlainTextRenderer {
    fn bold(&self, content: &str) -> String {
        content.to_string()
    }

    fn italic(&self, content: &str) -> String {
        content.to_string()
    }

    fn underline(&self, content: &str) -> String {
        content.to_string()
    }

    fn strike(&self, content: &str) -> String {
        content.to_string()
    }

    fn color(&self, _color: &str, content: &str) -> String {
        content.to_string()
    }

    fn size(&self, _size: &str, content: &str) -> String {
        content.to_string()
    }

    fn url(&self, url: &str, text: &str) -> String {
        if text == url {
            url.to_string()
        } else {
            format!("{} ({})", text, url)
        }
    }

    fn image(&self, url: &str) -> String {
        format!("[Image: {}]", url)
    }
}

/// ANSI terminal renderer
pub struct AnsiRenderer;

impl BBCodeRenderer for AnsiRenderer {
    fn bold(&self, content: &str) -> String {
        format!("\x1b[1m{}\x1b[0m", content)
    }

    fn italic(&self, content: &str) -> String {
        format!("\x1b[3m{}\x1b[0m", content)
    }

    fn underline(&self, content: &str) -> String {
        format!("\x1b[4m{}\x1b[0m", content)
    }

    fn strike(&self, content: &str) -> String {
        format!("\x1b[9m{}\x1b[0m", content)
    }

    fn color(&self, color: &str, content: &str) -> String {
        // Simple color mapping
        let ansi_code = match color.to_lowercase().as_str() {
            "red" => "31",
            "green" => "32",
            "yellow" => "33",
            "blue" => "34",
            "magenta" => "35",
            "cyan" => "36",
            "white" => "37",
            _ => "39", // default
        };
        format!("\x1b[{}m{}\x1b[0m", ansi_code, content)
    }

    fn size(&self, _size: &str, content: &str) -> String {
        content.to_string()
    }

    fn url(&self, url: &str, text: &str) -> String {
        // OSC 8 hyperlink if supported
        format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", url, text)
    }

    fn image(&self, url: &str) -> String {
        format!("[Image: {}]", url)
    }
}

impl BBCodeParser {
    /// Parse and render BBCode using the given renderer
    pub fn render<R: BBCodeRenderer>(input: &str, renderer: &R) -> String {
        let mut output = input.to_string();

        // Process tags from innermost to outermost
        output = Self::process_simple_tag(&output, "b", |c| renderer.bold(c));
        output = Self::process_simple_tag(&output, "i", |c| renderer.italic(c));
        output = Self::process_simple_tag(&output, "u", |c| renderer.underline(c));
        output = Self::process_simple_tag(&output, "s", |c| renderer.strike(c));

        output = Self::process_param_tag(&output, "color", |p, c| renderer.color(p, c));
        output = Self::process_param_tag(&output, "size", |p, c| renderer.size(p, c));
        output = Self::process_param_tag(&output, "url", |p, c| renderer.url(p, c));

        // Simple URL tags
        output = Self::process_simple_tag(&output, "url", |c| renderer.url(c, c));

        // Images
        output = Self::process_simple_tag(&output, "img", |c| renderer.image(c));

        output
    }

    /// Convert BBCode to HTML
    pub fn to_html(input: &str) -> String {
        Self::render(input, &HtmlRenderer)
    }

    /// Convert BBCode to plain text
    pub fn to_plain(input: &str) -> String {
        Self::render(input, &PlainTextRenderer)
    }

    /// Convert BBCode to ANSI terminal output
    pub fn to_ansi(input: &str) -> String {
        Self::render(input, &AnsiRenderer)
    }

    fn process_simple_tag<F>(input: &str, tag: &str, renderer: F) -> String
    where
        F: Fn(&str) -> String,
    {
        let pattern = format!(r"\[{}\](.*?)\[/{}\]", tag, tag);
        let re = Regex::new(&pattern).unwrap();
        re.replace_all(input, |caps: &regex::Captures| renderer(&caps[1]))
            .to_string()
    }

    fn process_param_tag<F>(input: &str, tag: &str, renderer: F) -> String
    where
        F: Fn(&str, &str) -> String,
    {
        let pattern = format!(r"\[{}=([^\]]+)\](.*?)\[/{}\]", tag, tag);
        let re = Regex::new(&pattern).unwrap();
        re.replace_all(input, |caps: &regex::Captures| renderer(&caps[1], &caps[2]))
            .to_string()
    }
}

/// Strip all BBCode tags from text
pub fn strip_bbcode(input: &str) -> String {
    BBCodeParser::to_plain(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bold() {
        assert_eq!(
            BBCodeParser::to_html("[b]test[/b]"),
            "<strong>test</strong>"
        );
    }

    #[test]
    fn test_color() {
        assert_eq!(
            BBCodeParser::to_html("[color=red]test[/color]"),
            "<span style=\"color: red\">test</span>"
        );
    }

    #[test]
    fn test_strip() {
        assert_eq!(strip_bbcode("[b]hello[/b] [i]world[/i]"), "hello world");
    }

    #[test]
    fn italic_html() {
        assert_eq!(BBCodeParser::to_html("[i]x[/i]"), "<em>x</em>");
    }

    #[test]
    fn underline_html() {
        assert_eq!(BBCodeParser::to_html("[u]x[/u]"), "<u>x</u>");
    }

    #[test]
    fn strike_html() {
        assert_eq!(BBCodeParser::to_html("[s]x[/s]"), "<s>x</s>");
    }

    #[test]
    fn url_with_param() {
        assert_eq!(
            BBCodeParser::to_html("[url=https://ex.com]click[/url]"),
            "<a href=\"https://ex.com\">click</a>"
        );
    }

    #[test]
    fn url_simple() {
        assert_eq!(
            BBCodeParser::to_html("[url]https://ex.com[/url]"),
            "<a href=\"https://ex.com\">https://ex.com</a>"
        );
    }

    #[test]
    fn image_tag() {
        assert_eq!(
            BBCodeParser::to_html("[img]pic.png[/img]"),
            "<img src=\"pic.png\" />"
        );
    }

    #[test]
    fn nested_bold_italic() {
        // Inner tags processed first
        let result = BBCodeParser::to_html("[b][i]text[/i][/b]");
        assert_eq!(result, "<strong><em>text</em></strong>");
    }

    #[test]
    fn plain_text_url_different_text() {
        let result = BBCodeParser::to_plain("[url=https://ex.com]click[/url]");
        assert_eq!(result, "click (https://ex.com)");
    }

    #[test]
    fn plain_text_url_same_text() {
        let result = BBCodeParser::to_plain("[url]https://ex.com[/url]");
        assert_eq!(result, "https://ex.com");
    }

    #[test]
    fn plain_text_image() {
        assert_eq!(BBCodeParser::to_plain("[img]pic.png[/img]"), "[Image: pic.png]");
    }

    #[test]
    fn ansi_bold() {
        let result = BBCodeParser::to_ansi("[b]hi[/b]");
        assert_eq!(result, "\x1b[1mhi\x1b[0m");
    }

    #[test]
    fn ansi_color_red() {
        let result = BBCodeParser::to_ansi("[color=red]err[/color]");
        assert_eq!(result, "\x1b[31merr\x1b[0m");
    }

    #[test]
    fn ansi_unknown_color_uses_default() {
        let result = BBCodeParser::to_ansi("[color=#ff0000]x[/color]");
        assert_eq!(result, "\x1b[39mx\x1b[0m");
    }

    #[test]
    fn no_tags_unchanged() {
        assert_eq!(BBCodeParser::to_html("plain text"), "plain text");
        assert_eq!(BBCodeParser::to_plain("plain text"), "plain text");
    }

    #[test]
    fn malformed_tags_unchanged() {
        // Unclosed tags remain as-is
        assert_eq!(BBCodeParser::to_html("[b]open"), "[b]open");
    }

    #[test]
    fn size_html() {
        assert_eq!(
            BBCodeParser::to_html("[size=12]big[/size]"),
            "<span style=\"font-size: 12px\">big</span>"
        );
    }
}
