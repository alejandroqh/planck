use floem::text::{
    Attrs, AttrsList, FamilyOwned, LineHeightValue, Style, TextLayout, Weight,
};
use lapce_xi_rope::Rope;
use lsp_types::MarkedString;
use planck_core::{language::PlanckLanguage, syntax::Syntax};
use pulldown_cmark::{CodeBlockKind, CowStr, Event, Options, Parser, Tag};
use smallvec::SmallVec;

use crate::config::{PlanckConfig, color::PlanckColor};

#[derive(Clone)]
pub enum MarkdownContent {
    Text(TextLayout),
    Image { url: String, title: String },
    Separator,
}

pub fn parse_markdown(
    text: &str,
    line_height: f64,
    config: &PlanckConfig,
) -> Vec<MarkdownContent> {
    let mut res = Vec::new();

    let mut current_text = String::new();
    let code_font_family: Vec<FamilyOwned> =
        FamilyOwned::parse_list(&config.editor.font_family).collect();

    let default_attrs = Attrs::new()
        .color(config.color(PlanckColor::EDITOR_FOREGROUND))
        .font_size(config.ui.font_size() as f32)
        .line_height(LineHeightValue::Normal(line_height as f32));
    let mut attr_list = AttrsList::new(default_attrs.clone());

    let mut builder_dirty = false;

    let mut pos = 0;

    let mut tag_stack: SmallVec<[(usize, Tag); 4]> = SmallVec::new();

    let parser = Parser::new_ext(text, markdown_options());
    // Ordered lists keep their next number, bullet lists None.
    let mut lists: SmallVec<[Option<u64>; 4]> = SmallVec::new();
    let mut last_text = CowStr::from("");
    // Whether we should add a newline on the next entry
    // This is used so that we don't emit newlines at the very end of the generation
    let mut add_newline = false;
    for event in parser {
        // Add the newline since we're going to be outputting more.
        // Nested blocks end together, so don't stack blank lines.
        if add_newline {
            if !current_text.ends_with('\n') {
                current_text.push('\n');
                builder_dirty = true;
                pos += 1;
            }
            add_newline = false;
        }

        match event {
            Event::Start(tag) => {
                match &tag {
                    Tag::List(start) => lists.push(*start),
                    Tag::Item => {
                        if !current_text.is_empty() && !current_text.ends_with('\n') {
                            current_text.push('\n');
                            pos += 1;
                        }
                        let marker = match lists.last_mut() {
                            Some(Some(n)) => {
                                *n += 1;
                                format!("{}. ", *n - 1)
                            }
                            _ => "• ".to_string(),
                        };
                        let indent = "    ".repeat(lists.len().saturating_sub(1));
                        current_text.push_str(&indent);
                        current_text.push_str(&marker);
                        pos += indent.len() + marker.len();
                        builder_dirty = true;
                    }
                    _ => {}
                }
                tag_stack.push((pos, tag));
            }
            Event::End(end_tag) => {
                if let Some((start_offset, tag)) = tag_stack.pop() {
                    if end_tag != tag.to_end() {
                        tracing::warn!("Mismatched markdown tag");
                        continue;
                    }

                    if let Some(attrs) = attribute_for_tag(
                        default_attrs.clone(),
                        &tag,
                        &code_font_family,
                        config,
                    ) {
                        attr_list
                            .add_span(start_offset..pos.max(start_offset), attrs);
                    }

                    if should_add_newline_after_tag(&tag) {
                        add_newline = true;
                    }
                    if let Tag::List(_) = tag {
                        lists.pop();
                    }

                    match &tag {
                        Tag::CodeBlock(kind) => {
                            let language =
                                if let CodeBlockKind::Fenced(language) = kind {
                                    md_language_to_planck_language(language)
                                } else {
                                    None
                                };

                            highlight_as_code(
                                &mut attr_list,
                                default_attrs.clone().family(&code_font_family),
                                language,
                                &last_text,
                                start_offset,
                                config,
                            );
                            builder_dirty = true;
                        }
                        Tag::Image {
                            link_type: _,
                            dest_url: dest,
                            title,
                            id: _,
                        } => {
                            // TODO: Are there any link types that would change how the
                            // image is rendered?

                            if builder_dirty {
                                let mut text_layout = TextLayout::new();
                                text_layout.set_text(&current_text, attr_list, None);
                                res.push(MarkdownContent::Text(text_layout));
                                attr_list = AttrsList::new(default_attrs.clone());
                                current_text.clear();
                                pos = 0;
                                builder_dirty = false;
                            }

                            res.push(MarkdownContent::Image {
                                url: dest.to_string(),
                                title: title.to_string(),
                            });
                        }
                        _ => {
                            // Presumably?
                            builder_dirty = true;
                        }
                    }
                } else {
                    tracing::warn!("Unbalanced markdown tag")
                }
            }
            Event::Text(text) => {
                if let Some((_, tag)) = tag_stack.last() {
                    if should_skip_text_in_tag(tag) {
                        continue;
                    }
                }
                current_text.push_str(&text);
                pos += text.len();
                last_text = text;
                builder_dirty = true;
            }
            Event::Code(text) => {
                attr_list.add_span(
                    pos..pos + text.len(),
                    default_attrs.clone().family(&code_font_family),
                );
                current_text.push_str(&text);
                pos += text.len();
                builder_dirty = true;
            }
            // TODO: Some minimal 'parsing' of html could be useful here, since some things use
            // basic html like `<code>text</code>`.
            Event::Html(text) => {
                attr_list.add_span(
                    pos..pos + text.len(),
                    default_attrs
                        .clone()
                        .family(&code_font_family)
                        .color(config.color(PlanckColor::MARKDOWN_BLOCKQUOTE)),
                );
                current_text.push_str(&text);
                pos += text.len();
                builder_dirty = true;
            }
            Event::HardBreak => {
                current_text.push('\n');
                pos += 1;
                builder_dirty = true;
            }
            Event::SoftBreak => {
                current_text.push(' ');
                pos += 1;
                builder_dirty = true;
            }
            Event::Rule => {}
            Event::FootnoteReference(_text) => {}
            Event::TaskListMarker(checked) => {
                let marker = if checked { "☑ " } else { "☐ " };
                current_text.push_str(marker);
                pos += marker.len();
                builder_dirty = true;
            }
            Event::InlineHtml(_) => {} // TODO(panekj): Implement
            Event::InlineMath(_) => {} // TODO(panekj): Implement
            Event::DisplayMath(_) => {} // TODO(panekj): Implement
        }
    }

    if builder_dirty {
        let mut text_layout = TextLayout::new();
        text_layout.set_text(&current_text, attr_list, None);
        res.push(MarkdownContent::Text(text_layout));
    }

    res
}

/// Which part of markdown source a span is, for [`style_markdown_source`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SourcePart {
    /// Syntax such as `##`, `**`, list markers and link targets.
    Markup,
    Text,
    Code,
}

/// Walks markdown source in order, calling `emit` for every span with the
/// tags it is inside, so the whole text is covered once.
pub fn walk_source(
    text: &str,
    mut emit: impl FnMut(std::ops::Range<usize>, &[Tag], SourcePart),
) {
    let mut stack: Vec<Tag> = Vec::new();
    let mut pos = 0;
    let mut part = |range: std::ops::Range<usize>,
                    stack: &[Tag],
                    kind: SourcePart,
                    pos: &mut usize| {
        if range.start < range.end && range.start >= *pos {
            emit(range.clone(), stack, kind);
        }
        *pos = (*pos).max(range.end);
    };
    for (event, range) in Parser::new_ext(text, document_options()).into_offset_iter()
    {
        match event {
            Event::Start(tag) => {
                part(pos..range.start, &stack, SourcePart::Markup, &mut pos);
                stack.push(tag);
            }
            Event::End(_) => {
                part(pos..range.end, &stack, SourcePart::Markup, &mut pos);
                stack.pop();
            }
            Event::Text(_) | Event::Html(_) | Event::InlineHtml(_) => {
                part(pos..range.start, &stack, SourcePart::Markup, &mut pos);
                part(range, &stack, SourcePart::Text, &mut pos);
            }
            Event::Code(_) => {
                part(pos..range.start, &stack, SourcePart::Markup, &mut pos);
                let code = &text[range.clone()];
                let lead = code.len() - code.trim_start_matches('`').len();
                let trail = code.len() - code.trim_end_matches('`').len();
                let inner = range.start + lead..range.end - trail;
                part(range.start..inner.start, &stack, SourcePart::Markup, &mut pos);
                part(inner, &stack, SourcePart::Code, &mut pos);
                part(pos..range.end, &stack, SourcePart::Markup, &mut pos);
            }
            // Breaks, rules and task markers are covered by the next markup span.
            _ => {}
        }
    }
    part(pos..text.len(), &stack, SourcePart::Markup, &mut pos);
}

/// Markdown source styled like its rendering (heading sizes, bold, code font),
/// with the syntax characters dimmed, for editing in the Rich view.
pub fn style_markdown_source(
    text: &str,
    line_height: f64,
    config: &PlanckConfig,
) -> AttrsList {
    let code_font_family: Vec<FamilyOwned> =
        FamilyOwned::parse_list(&config.editor.font_family).collect();
    let default_attrs = Attrs::new()
        .color(config.color(PlanckColor::EDITOR_FOREGROUND))
        .font_size(config.ui.font_size() as f32)
        .line_height(LineHeightValue::Normal(line_height as f32));
    let dim = config.color(PlanckColor::EDITOR_DIM);
    let mut attr_list = AttrsList::new(default_attrs.clone());
    walk_source(text, |range, stack, part| {
        let attrs = stack.iter().fold(default_attrs.clone(), |attrs, tag| {
            attribute_for_tag(attrs.clone(), tag, &code_font_family, config)
                .unwrap_or(attrs)
        });
        match part {
            SourcePart::Markup => attr_list.add_span(range, attrs.color(dim)),
            SourcePart::Code => {
                attr_list.add_span(range, attrs.family(&code_font_family))
            }
            SourcePart::Text => {
                let language = stack.iter().find_map(|tag| match tag {
                    Tag::CodeBlock(CodeBlockKind::Fenced(lang)) => Some(lang),
                    _ => None,
                });
                attr_list.add_span(range.clone(), attrs.clone());
                if let Some(language) = language {
                    highlight_as_code(
                        &mut attr_list,
                        attrs,
                        md_language_to_planck_language(language),
                        &text[range.clone()],
                        range.start,
                        config,
                    );
                }
            }
        }
    });
    attr_list
}

pub fn markdown_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
}

/// Options for whole markdown files, which can start with YAML front matter.
/// Hover text from language servers is parsed without it.
pub fn document_options() -> Options {
    markdown_options() | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
}

fn attribute_for_tag<'a>(
    default_attrs: Attrs<'a>,
    tag: &Tag,
    code_font_family: &'a [FamilyOwned],
    config: &PlanckConfig,
) -> Option<Attrs<'a>> {
    use pulldown_cmark::HeadingLevel;
    match tag {
        Tag::Heading {
            level,
            id: _,
            classes: _,
            attrs: _,
        } => {
            // The size calculations are based on the em values given at
            // https://drafts.csswg.org/css2/#html-stylesheet
            let font_scale = match level {
                HeadingLevel::H1 => 2.0,
                HeadingLevel::H2 => 1.5,
                HeadingLevel::H3 => 1.17,
                HeadingLevel::H4 => 1.0,
                HeadingLevel::H5 => 0.83,
                HeadingLevel::H6 => 0.75,
            };
            let font_size = font_scale * config.ui.font_size() as f64;
            Some(
                default_attrs
                    .font_size(font_size as f32)
                    .weight(Weight::BOLD),
            )
        }
        Tag::BlockQuote(_block_quote) => Some(
            default_attrs
                .style(Style::Italic)
                .color(config.color(PlanckColor::MARKDOWN_BLOCKQUOTE)),
        ),
        Tag::CodeBlock(_) => Some(default_attrs.family(code_font_family)),
        Tag::Emphasis => Some(default_attrs.style(Style::Italic)),
        Tag::Strong => Some(default_attrs.weight(Weight::BOLD)),
        // TODO: Strikethrough support
        Tag::Link {
            link_type: _,
            dest_url: _,
            title: _,
            id: _,
        } => {
            // TODO: Link support
            Some(default_attrs.color(config.color(PlanckColor::EDITOR_LINK)))
        }
        // All other tags are currently ignored
        _ => None,
    }
}

/// Decides whether newlines should be added after a specific markdown tag
fn should_add_newline_after_tag(tag: &Tag) -> bool {
    !matches!(
        tag,
        Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. }
    )
}

/// Whether it should skip the text node after a specific tag  
/// For example, images are skipped because it emits their title as a separate text node.  
fn should_skip_text_in_tag(tag: &Tag) -> bool {
    matches!(tag, Tag::Image { .. })
}

fn md_language_to_planck_language(lang: &str) -> Option<PlanckLanguage> {
    // TODO: There are many other names commonly used that should be supported
    PlanckLanguage::from_name(lang)
}

/// Highlight the text in a richtext builder like it was a markdown codeblock
pub fn highlight_as_code(
    attr_list: &mut AttrsList,
    default_attrs: Attrs,
    language: Option<PlanckLanguage>,
    text: &str,
    start_offset: usize,
    config: &PlanckConfig,
) {
    let syntax = language.map(Syntax::from_language);

    let styles = syntax
        .map(|mut syntax| {
            syntax.parse(0, Rope::from(text), None);
            syntax.styles
        })
        .unwrap_or(None);

    if let Some(styles) = styles {
        for (range, style) in styles.iter() {
            if let Some(color) = style
                .fg_color
                .as_ref()
                .and_then(|fg| config.style_color(fg))
            {
                attr_list.add_span(
                    start_offset + range.start..start_offset + range.end,
                    default_attrs.clone().color(color),
                );
            }
        }
    }
}

pub fn from_marked_string(
    text: MarkedString,
    config: &PlanckConfig,
) -> Vec<MarkdownContent> {
    match text {
        MarkedString::String(text) => parse_markdown(&text, 1.8, config),
        // This is a short version of a code block
        MarkedString::LanguageString(code) => {
            // TODO: We could simply construct the MarkdownText directly
            // Simply construct the string as if it was written directly
            parse_markdown(
                &format!("```{}\n{}\n```", code.language, code.value),
                1.8,
                config,
            )
        }
    }
}

pub fn from_plaintext(
    text: &str,
    line_height: f64,
    config: &PlanckConfig,
) -> Vec<MarkdownContent> {
    let mut text_layout = TextLayout::new();
    text_layout.set_text(
        text,
        AttrsList::new(
            Attrs::new()
                .font_size(config.ui.font_size() as f32)
                .line_height(LineHeightValue::Normal(line_height as f32)),
        ),
        None,
    );
    vec![MarkdownContent::Text(text_layout)]
}

#[cfg(test)]
mod tests {
    use super::{SourcePart, walk_source};

    fn markup(text: &str) -> Vec<&str> {
        let mut spans = Vec::new();
        let mut covered = 0;
        walk_source(text, |range, _, part| {
            assert_eq!(range.start, covered, "spans must be contiguous");
            covered = range.end;
            if part == SourcePart::Markup && !text[range.clone()].trim().is_empty() {
                spans.push(text[range].trim());
            }
        });
        assert_eq!(covered, text.len());
        spans
    }

    #[test]
    fn markdown_syntax_is_found() {
        assert_eq!(markup("## Title\n"), ["##"]);
        assert_eq!(markup("a **b** `c` [d](e)\n"), ["**", "**", "`", "`", "[", "](e)"]);
        assert_eq!(markup("```rs\nfn x() {}\n```\n"), ["```rs", "```"]);
        assert_eq!(markup("- one\n"), ["-"]);
    }
}
