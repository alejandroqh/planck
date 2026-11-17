//! The Rich view of a markdown file: the same document and cursor as the text
//! editor, drawn as formatted blocks. The block holding the cursor shows its raw
//! markdown so it is edited in place, and keys go to the editor as usual.

use std::{
    cell::RefCell,
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use floem::{
    AnyView, IntoView, Renderer, View,
    context::PaintCx,
    event::{Event, EventListener, EventPropagation},
    peniko::{
        Color,
        kurbo::{Point, Rect},
    },
    pointer::{MouseButton, PointerInputEvent},
    ext_event::create_ext_action,
    reactive::{
        Memo, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith, create_effect,
        create_memo, create_rw_signal,
    },
    style::CursorStyle,
    text::{Attrs, AttrsList, LineHeightValue, TextLayout},
    views::{
        Decorators, canvas, container, dyn_container, dyn_stack, empty, label,
        rich_text, scroll, stack, stack_from_iter, v_stack_from_iter,
    },
};
use planck_core::{cursor::CursorMode, editor::EditType, selection::Selection};
use pulldown_cmark::{BlockQuoteKind, Event as MdEvent, Parser, Tag, TagEnd};

use super::EditorData;
use crate::{
    command::InternalCommand,
    config::{PlanckConfig, color::PlanckColor},
    markdown::{
        MarkdownContent, STRIKETHROUGH, document_options, markdown_options,
        parse_markdown, style_markdown_source,
    },
    window_tab::Focus,
};

/// Line spacing of rendered blocks, and of the block being edited so it doesn't jump.
const RENDERED_LINE_HEIGHT: f64 = 1.6;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum BlockKind {
    Rule,
    Code,
    Table,
    FrontMatter,
    /// A blockquote, or a GitHub alert (`> [!NOTE]`) with its kind.
    Quote(Option<BlockQuoteKind>),
    Other,
}

#[derive(Clone, PartialEq, Debug)]
struct Block {
    range: Range<usize>,
    kind: BlockKind,
}

pub fn markdown_rich_view(
    e_data: RwSignal<EditorData>,
    is_active: impl Fn(bool) -> bool + 'static + Copy,
) -> impl View {
    let ed = e_data.get_untracked();
    let config = ed.common.config;
    let doc = ed.doc_signal();
    let cursor = ed.cursor();

    let text = create_memo(move |_| doc.with(|d| d.buffer.with(|b| b.to_string())));
    let blocks = create_memo(move |_| text.with(|t| split_blocks(t)));

    // Raw markdown only shows once the cursor moves or the view is clicked, so a
    // file switched to Rich starts fully rendered.
    let revealed = create_rw_signal(false);
    create_effect(move |prev: Option<usize>| {
        let offset = cursor.with(|c| c.offset());
        if prev.is_some_and(|p| p != offset) {
            revealed.set(true);
        }
        offset
    });
    let editing = create_memo(move |_| {
        if !(is_active(true) && revealed.get()) {
            return None;
        }
        let offset = cursor.with(|c| c.offset());
        Some(blocks.with(|b| block_at(b, offset)))
    });

    let base_dir = ed
        .doc()
        .content
        .with_untracked(|c| c.path().and_then(|p| p.parent()).map(Path::to_path_buf));
    let ed_ime = ed.clone();
    scroll(
        dyn_stack(
            move || {
                // A new config (theme, fonts) rebuilds every block.
                let config_id = config.with(|c| Arc::as_ptr(c) as usize);
                let editing = editing.get();
                let text = text.get();
                let blocks = blocks.get();
                let count = blocks.len();
                blocks
                    .into_iter()
                    .enumerate()
                    .map(|(i, b)| BlockItem {
                        index: i,
                        text: text[b.range].to_string(),
                        kind: b.kind,
                        raw: editing == Some(i),
                        last: i + 1 == count,
                        config_id,
                    })
                    .collect::<Vec<_>>()
            },
            |item| item.clone(),
            move |item| {
                let ed = ed.clone();
                if item.raw {
                    raw_block(ed, blocks, item, is_active).into_any()
                } else {
                    let ctx = BlockCtx {
                        ed,
                        blocks,
                        index: item.index,
                        revealed,
                    };
                    rendered_block(ctx, item, base_dir.clone()).into_any()
                }
            },
        )
        .style(|s| {
            s.flex_col()
                .width_full()
                .padding_horiz(32.0)
                .padding_top(16.0)
                .padding_bottom(200.0)
        }),
    )
    .on_event(EventListener::ImeCommit, move |event| {
        if !is_active(false) {
            return EventPropagation::Continue;
        }
        if let Event::ImeCommit(text) = event {
            ed_ime.editor.clear_preedit();
            ed_ime.editor.receive_char(text);
        }
        EventPropagation::Stop
    })
    .style(move |s| {
        // Absolute, so the content height doesn't squeeze the path bar above.
        s.absolute()
            .size_full()
            .cursor(CursorStyle::Text)
            .background(config.get().color(PlanckColor::EDITOR_BACKGROUND))
    })
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct BlockItem {
    index: usize,
    text: String,
    kind: BlockKind,
    raw: bool,
    last: bool,
    config_id: usize,
}

fn block_start(blocks: Memo<Vec<Block>>, index: usize) -> usize {
    blocks.with_untracked(|b| b.get(index).map_or(0, |b| b.range.start))
}

/// What a rendered block needs to turn a click into a cursor position.
#[derive(Clone)]
struct BlockCtx {
    ed: EditorData,
    blocks: Memo<Vec<Block>>,
    index: usize,
    revealed: RwSignal<bool>,
}

impl BlockCtx {
    /// Places the cursor at `offset` within this block's source.
    fn click_at(&self, offset: usize, pe: &PointerInputEvent) {
        let offset = block_start(self.blocks, self.index) + offset;
        if follow_link(&self.ed, offset, pe) {
            return;
        }
        self.revealed.set(true);
        click(&self.ed, offset, pe);
    }
}

/// A formatted block. Clicking it puts the cursor at the matching spot in the source.
fn rendered_block(ctx: BlockCtx, item: BlockItem, base_dir: Option<PathBuf>) -> impl View {
    let config = ctx.ed.common.config.get_untracked();
    let source: Arc<str> = Arc::from(item.text.as_str());
    let body = match item.kind {
        BlockKind::Table => table_view(&ctx, &source, &config),
        BlockKind::FrontMatter => front_matter_view(&ctx, &source, &config),
        BlockKind::Rule => separator(&config),
        BlockKind::Code | BlockKind::Quote(_) | BlockKind::Other => {
            let mut rendered_before = String::new();
            let pieces = parse_markdown(&source, RENDERED_LINE_HEIGHT, &config)
                .into_iter()
                .map(|piece| match piece {
                    MarkdownContent::Text(layout) => {
                        let prefix = rendered_before.clone();
                        rendered_before.push_str(&rendered_text(&layout));
                        rendered_before.push('\n');
                        linked_text(ctx.clone(), layout, source.clone(), 0, prefix)
                            .into_any()
                    }
                    MarkdownContent::Separator => separator(&config),
                    MarkdownContent::Image { url, .. } => {
                        image_view(url, base_dir.clone(), &config)
                    }
                })
                .collect::<Vec<_>>();
            let body = stack_from_iter(pieces)
                .style(|s| s.flex_col().width_full())
                .into_any();
            match item.kind {
                BlockKind::Quote(kind) => quote_view(body, kind, &config),
                _ => body,
            }
        }
    };

    let line_height = config.editor.line_height() as f32;
    let code_background = config.color(PlanckColor::EDITOR_CURRENT_LINE);
    let is_code = item.kind == BlockKind::Code;
    container(body)
        .on_event_stop(EventListener::PointerDown, move |event| {
            if let Event::PointerDown(pe) = event {
                ctx.click_at(0, pe);
            }
        })
        .style(move |s| {
            s.width_full()
                .min_height(line_height)
                .padding_vert(4.0)
                .apply_if(is_code, |s| {
                    s.padding_horiz(10.0)
                        .margin_vert(4.0)
                        .border_radius(4.0)
                        .background(code_background)
                })
        })
}

/// The bar color and title of a blockquote or GitHub alert.
fn quote_style(
    kind: Option<BlockQuoteKind>,
    config: &PlanckConfig,
) -> (Color, Option<&'static str>) {
    let (color, title) = match kind {
        None => return (config.color(PlanckColor::MARKDOWN_BLOCKQUOTE), None),
        Some(BlockQuoteKind::Note) => (PlanckColor::TERMINAL_BLUE, "Note"),
        Some(BlockQuoteKind::Tip) => (PlanckColor::TERMINAL_GREEN, "Tip"),
        Some(BlockQuoteKind::Important) => (PlanckColor::TERMINAL_MAGENTA, "Important"),
        Some(BlockQuoteKind::Warning) => (PlanckColor::TERMINAL_YELLOW, "Warning"),
        Some(BlockQuoteKind::Caution) => (PlanckColor::TERMINAL_RED, "Caution"),
    };
    (config.color(color), Some(title))
}

fn quote_bar(s: floem::style::Style, color: Color) -> floem::style::Style {
    s.border_left(3.0).border_color(color).padding_left(12.0)
}

fn quote_view(
    body: AnyView,
    kind: Option<BlockQuoteKind>,
    config: &PlanckConfig,
) -> AnyView {
    let (color, title) = quote_style(kind, config);
    let mut parts = Vec::new();
    if let Some(title) = title {
        parts.push(
            label(move || title)
                .style(move |s| s.color(color).font_bold().padding_bottom(2.0))
                .into_any(),
        );
    }
    parts.push(body);
    v_stack_from_iter(parts)
        .style(move |s| quote_bar(s.width_full(), color))
        .into_any()
}

fn separator(config: &PlanckConfig) -> AnyView {
    let border = config.color(PlanckColor::PLANCK_BORDER);
    empty()
        .style(move |s| s.width_full().margin_vert(8.0).height(1.0).background(border))
        .into_any()
}

/// Rendered text from `source[base..]`: a click maps back into the source.
/// `prefix` is the rendered text drawn before this layout from the same source.
fn linked_text(
    ctx: BlockCtx,
    layout: TextLayout,
    source: Arc<str>,
    base: usize,
    prefix: String,
) -> impl View {
    let layout = create_rw_signal(layout);
    wrapped_text(layout, create_rw_signal(-1.0)).on_event_stop(EventListener::PointerDown, move |event| {
        let Event::PointerDown(pe) = event else { return };
        let Some((before, after)) = layout.with_untracked(|l| {
            let hit = hit_offset(l, pe.pos)?;
            let rendered = rendered_text(l);
            let (before, after) = rendered.split_at_checked(hit)?;
            Some((format!("{prefix}{before}"), after.to_string()))
        }) else {
            return;
        };
        if pe.button.mouse_button() == MouseButton::Primary
            && let Some(task) = clicked_task(&before, &after)
        {
            toggle_task(&ctx, &source, base, task);
            return;
        }
        ctx.click_at(base + source_offset(&source, &before), pe);
    })
}

/// One grid cell for tables and front matter; columns share the width by `weight`.
fn grid_cell(
    content: impl IntoView + 'static,
    weight: f32,
    header: bool,
    config: &PlanckConfig,
    on_click: impl Fn(&PointerInputEvent) + 'static,
) -> AnyView {
    let border = config.color(PlanckColor::PLANCK_BORDER);
    let header_background = config.color(PlanckColor::EDITOR_CURRENT_LINE);
    container(content)
        .on_event_stop(EventListener::PointerDown, move |event| {
            if let Event::PointerDown(pe) = event {
                on_click(pe);
            }
        })
        .style(move |s| {
            s.flex_basis(0)
                .flex_grow(weight)
                .min_width(0)
                .padding_horiz(8.0)
                .padding_vert(2.0)
                .border_right(1.0)
                .border_bottom(1.0)
                .border_color(border)
                .apply_if(header, |s| s.background(header_background))
        })
        .into_any()
}

fn grid(rows: Vec<AnyView>, config: &PlanckConfig) -> AnyView {
    let border = config.color(PlanckColor::PLANCK_BORDER);
    v_stack_from_iter(rows)
        .style(move |s| {
            s.width_full()
                .margin_vert(4.0)
                .border_left(1.0)
                .border_top(1.0)
                .border_color(border)
        })
        .into_any()
}

/// Column weight from the longest cell, so short columns stay narrow.
fn column_weight(lengths: impl Iterator<Item = usize>) -> f32 {
    lengths.max().unwrap_or(0).clamp(3, 40) as f32
}

fn table_view(ctx: &BlockCtx, source: &Arc<str>, config: &PlanckConfig) -> AnyView {
    let rows = table_rows(source);
    let columns = rows.iter().map(|(_, cells)| cells.len()).max().unwrap_or(0);
    let weights: Vec<f32> = (0..columns)
        .map(|c| {
            column_weight(
                rows.iter()
                    .filter_map(|(_, cells)| cells.get(c))
                    .map(|r| source[r.clone()].chars().count()),
            )
        })
        .collect();
    // shortcut: column alignment (:---:) is ignored, cells are left-aligned.
    let rows = rows
        .into_iter()
        .map(|(header, cells)| {
            let row = (0..columns).map(|c| {
                let range = cells.get(c).cloned().unwrap_or(0..0);
                let text = &source[range.clone()];
                let markdown = if header && !text.is_empty() {
                    format!("**{text}**")
                } else {
                    text.to_string()
                };
                let cell_source: Arc<str> = Arc::from(text);
                let mut rendered_before = String::new();
                let lines = parse_markdown(&markdown, RENDERED_LINE_HEIGHT, config)
                    .into_iter()
                    .filter_map(|piece| match piece {
                        MarkdownContent::Text(layout) => {
                            let prefix = rendered_before.clone();
                            rendered_before.push_str(&rendered_text(&layout));
                            Some(linked_text(
                                ctx.clone(),
                                layout,
                                cell_source.clone(),
                                range.start,
                                prefix,
                            ))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let ctx = ctx.clone();
                grid_cell(
                    v_stack_from_iter(lines).style(|s| s.width_full()),
                    weights[c],
                    header,
                    config,
                    move |pe| ctx.click_at(range.start, pe),
                )
            });
            stack_from_iter(row).style(|s| s.width_full()).into_any()
        })
        .collect();
    grid(rows, config)
}

fn front_matter_view(
    ctx: &BlockCtx,
    source: &Arc<str>,
    config: &PlanckConfig,
) -> AnyView {
    let rows = front_matter_rows(source);
    let key_weight = column_weight(rows.iter().map(|(k, _)| k.len()));
    let value_weight = column_weight(rows.iter().map(|(_, v)| v.len())).max(key_weight * 2.0);
    let dim = config.color(PlanckColor::EDITOR_DIM);
    let foreground = config.color(PlanckColor::EDITOR_FOREGROUND);
    let attrs = Attrs::new()
        .font_size(config.ui.font_size() as f32)
        .line_height(LineHeightValue::Normal(RENDERED_LINE_HEIGHT as f32));
    let cell = |range: Range<usize>, color: Color, weight: f32| {
        let layout = TextLayout::new_with_text(
            &source[range.clone()],
            AttrsList::new(attrs.clone().color(color)),
            None,
        );
        let text = linked_text(ctx.clone(), layout, Arc::from(&source[range.clone()]), range.start, String::new());
        let ctx = ctx.clone();
        grid_cell(text, weight, false, config, move |pe| ctx.click_at(range.start, pe))
    };
    let rows = rows
        .into_iter()
        .map(|(key, value)| {
            stack((cell(key, dim, key_weight), cell(value, foreground, value_weight)))
                .style(|s| s.width_full())
                .into_any()
        })
        .collect();
    grid(rows, config)
}

thread_local! {
    /// Remote images by URL, fetched once per run.
    static REMOTE_IMAGES: RefCell<HashMap<String, Option<Arc<Vec<u8>>>>> =
        RefCell::new(HashMap::new());
}

/// An image from a local path (relative to the markdown file) or an http(s) URL.
fn image_view(url: String, base_dir: Option<PathBuf>, config: &PlanckConfig) -> AnyView {
    // None while loading, Some(None) when it could not be read.
    let data: RwSignal<Option<Option<Arc<Vec<u8>>>>> = create_rw_signal(None);
    if url.starts_with("http://") || url.starts_with("https://") {
        match REMOTE_IMAGES.with(|c| c.borrow().get(&url).cloned()) {
            Some(cached) => data.set(Some(cached)),
            None => {
                let key = url.clone();
                let send = create_ext_action(Scope::current(), move |bytes: Option<Vec<u8>>| {
                    let bytes = bytes.map(Arc::new);
                    REMOTE_IMAGES.with(|c| c.borrow_mut().insert(key, bytes.clone()));
                    data.try_update(|d| *d = Some(bytes));
                });
                let url = url.clone();
                std::thread::spawn(move || send(fetch_image(&url)));
            }
        }
    } else {
        let path = url.strip_prefix("file://").unwrap_or(&url);
        let path = Path::new(path.split(['#', '?']).next().unwrap_or(path));
        let path = if path.is_relative() {
            base_dir.map(|dir| dir.join(path))
        } else {
            Some(path.to_path_buf())
        };
        data.set(Some(path.and_then(|p| std::fs::read(p).ok()).map(Arc::new)));
    }

    let width = create_rw_signal(0.0);
    let dim = config.color(PlanckColor::EDITOR_DIM);
    dyn_container(
        move || data.get(),
        move |state| match state {
            None => empty().into_any(),
            Some(Some(bytes)) => {
                sized_image(bytes, width).unwrap_or_else(|| missing_image(&url, dim))
            }
            Some(None) => missing_image(&url, dim),
        },
    )
    .on_resize(move |rect| {
        if rect.width() != width.get_untracked() {
            width.set(rect.width());
        }
    })
    .style(|s| s.width_full())
    .into_any()
}

fn missing_image(url: &str, color: Color) -> AnyView {
    let text = format!("[image: {url}]");
    label(move || text.clone())
        .style(move |s| s.color(color))
        .into_any()
}

fn fetch_image(url: &str) -> Option<Vec<u8>> {
    let user_agent = format!("Planck/{}", planck_core::meta::VERSION);
    let response = planck_proxy::get_url(url, Some(user_agent.as_str())).ok()?;
    let bytes = response.error_for_status().ok()?.bytes().ok()?;
    // Big downloads are not worth holding for a preview.
    (bytes.len() <= 20 << 20).then(|| bytes.to_vec())
}

/// The image at its natural size, shrunk to fit `width` with its proportions kept.
fn sized_image(bytes: Arc<Vec<u8>>, width: RwSignal<f64>) -> Option<AnyView> {
    let svg_text = std::str::from_utf8(&bytes)
        .ok()
        .filter(|t| t.contains("<svg"))
        .map(str::to_string);
    let (w, h) = match &svg_text {
        Some(text) => svg_size(text)?,
        None => {
            let (w, h) = image::io::Reader::new(std::io::Cursor::new(&bytes[..]))
                .with_guessed_format()
                .ok()?
                .into_dimensions()
                .ok()?;
            (w as f64, h as f64)
        }
    };
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let fit = move |s: floem::style::Style| {
        let available = width.get();
        let shown = if available > 0.0 { w.min(available) } else { w };
        s.width(shown).height(shown * h / w)
    };
    Some(match svg_text {
        Some(text) => floem::views::svg(move || text.clone()).style(fit).into_any(),
        None => floem::views::img(move || bytes.to_vec()).style(fit).into_any(),
    })
}

/// Width and height of an SVG from its root tag (width/height, else viewBox).
fn svg_size(svg: &str) -> Option<(f64, f64)> {
    let start = svg.find("<svg")?;
    let tag = &svg[start..start + svg[start..].find('>')?];
    let attr = |name: &str| {
        let re = regex::Regex::new(&format!(r#"\s{name}\s*=\s*["']([^"']*)["']"#)).ok()?;
        Some(re.captures(tag)?.get(1)?.as_str().to_string())
    };
    let number = |v: String| v.trim().trim_end_matches("px").parse::<f64>().ok();
    if let (Some(w), Some(h)) = (attr("width").and_then(number), attr("height").and_then(number)) {
        return Some((w, h));
    }
    let view_box: Vec<f64> = attr("viewBox")?
        .split([' ', ','])
        .filter_map(|n| n.parse().ok())
        .collect();
    match view_box[..] {
        [_, _, w, h] => Some((w, h)),
        _ => None,
    }
}

/// The block holding the cursor, shown as its markdown source with caret and selection.
fn raw_block(
    ed: EditorData,
    blocks: Memo<Vec<Block>>,
    item: BlockItem,
    is_active: impl Fn(bool) -> bool + 'static + Copy,
) -> impl View {
    let config = ed.common.config.get_untracked();
    let index = item.index;
    let len = item.text.len();
    let quote = match item.kind {
        BlockKind::Quote(kind) => Some(quote_style(kind, &config).0),
        _ => None,
    };
    let line_height = config.editor.line_height() as f64;
    // The last block can end in a newline with the cursor after it: give that line a glyph.
    let shown = if item.last && (item.text.is_empty() || item.text.ends_with('\n'))
    {
        format!("{} ", item.text)
    } else {
        item.text
    };
    let cursor = ed.cursor();
    // Only the cursor's line shows its markdown syntax; the rest of the block stays rendered.
    let text = shown.clone();
    let cursor_line = create_memo(move |_| {
        let offset = cursor.with(|c| c.offset());
        let offset = offset.checked_sub(block_start(blocks, index))?.min(len);
        Some(line_range(&text, offset))
    });
    let layout = create_rw_signal(TextLayout::new());
    let width = create_rw_signal(-1.0);
    create_effect(move |_| {
        let styles = style_markdown_source(
            &shown,
            RENDERED_LINE_HEIGHT,
            &config,
            cursor_line.get(),
        );
        let mut new_layout = TextLayout::new_with_text(&shown, styles, None);
        let width = width.get_untracked();
        if width > 0.0 {
            new_layout.set_size(width as f32, f32::MAX);
        }
        layout.set(new_layout);
    });

    let config = ed.common.config.get_untracked();
    let caret_color = config.color(PlanckColor::EDITOR_CARET);
    let selection_color = config.color(PlanckColor::EDITOR_SELECTION);
    let paint = canvas(move |cx, _| {
        let start = blocks.with(|b| b.get(index).map_or(0, |b| b.range.start));
        cursor.with(|c| {
            layout.with(|l| {
                if let CursorMode::Insert(selection) = &c.mode {
                    for region in selection.regions() {
                        let a = region.min().saturating_sub(start).min(len);
                        let b = region.max().saturating_sub(start).min(len);
                        if a < b {
                            paint_selection(cx, l, a..b, selection_color);
                        }
                    }
                }
                let offset = c.offset();
                if is_active(true) && (start..=start + len).contains(&offset) {
                    cx.fill(
                        &caret_rect(l, offset - start, line_height),
                        caret_color,
                        0.0,
                    );
                }
            })
        })
    })
    .style(|s| s.absolute().size_full());

    let view = stack((paint, wrapped_text(layout, width)));
    let id = view.id();
    create_effect(move |_| {
        let offset = cursor.with(|c| c.offset());
        let start = block_start(blocks, index);
        if (start..=start + len).contains(&offset) {
            let rect = layout.with(|l| caret_rect(l, offset - start, line_height));
            id.scroll_to(Some(rect.inflate(0.0, line_height)));
        }
    });

    let offset_at = move |pos: Point| {
        let hit = layout.with_untracked(|l| hit_offset(l, pos)).unwrap_or(len);
        block_start(blocks, index) + hit.min(len)
    };
    let ed_move = ed.clone();
    let ed_up = ed.clone();
    view.on_event_stop(EventListener::PointerDown, move |event| {
        if let Event::PointerDown(pe) = event {
            if pe.button.mouse_button() == MouseButton::Primary {
                id.request_active();
                ed.active().set(true);
            }
            let offset = offset_at(pe.pos);
            if !follow_link(&ed, offset, pe) {
                click(&ed, offset, pe);
            }
        }
    })
    .on_event_stop(EventListener::PointerMove, move |event| {
        if let Event::PointerMove(pe) = event
            && ed_move.active().get_untracked()
        {
            let offset = offset_at(pe.pos);
            ed_move
                .cursor()
                .update(|c| c.set_offset(offset, true, pe.modifiers.alt()));
        }
    })
    .on_event_stop(EventListener::PointerUp, move |_| {
        ed_up.active().set(false);
    })
    .style(move |s| {
        s.width_full()
            .min_height(line_height as f32)
            .padding_vert(4.0)
            .apply_opt(quote, quote_bar)
    })
}

/// Focuses this editor and places the cursor, like a click in the text view.
fn click(ed: &EditorData, offset: usize, pe: &PointerInputEvent) {
    ed.cancel_completion();
    ed.cancel_inline_completion();
    if let Some(editor_tab_id) = ed.editor_tab_id.get_untracked() {
        ed.common
            .internal_command
            .send(InternalCommand::FocusEditorTab { editor_tab_id });
    }
    ed.common.focus.set(Focus::Workbench);
    ed.find_focus.set(false);
    if pe.button.mouse_button() != MouseButton::Primary {
        return;
    }
    let (shift, alt) = (pe.modifiers.shift(), pe.modifiers.alt());
    let region = match pe.count {
        2 => Some(ed.editor.select_word(offset)),
        3 => {
            let line = ed.editor.line_of_offset(offset);
            Some((ed.editor.offset_of_line(line), ed.editor.offset_of_line(line + 1)))
        }
        _ => None,
    };
    ed.cursor().update(|c| match region {
        Some((start, end)) => c.add_region(start, end, shift, alt),
        None => c.set_offset(offset, shift, alt),
    });
}

/// A rich_text whose layout copy stays wrapped at the drawn width, so hit
/// testing and caret placement match what is on screen.
fn wrapped_text(layout: RwSignal<TextLayout>, width: RwSignal<f64>) -> impl View {
    let text = rich_text(move || layout.get())
        .on_resize(move |rect| {
            if rect.width() != width.get_untracked() {
                width.set(rect.width());
                layout.update(|l| l.set_size(rect.width() as f32, f32::MAX));
            }
        })
        .style(|s| s.width_full());
    let strikes = canvas(move |cx, _| layout.with(|l| paint_strikethrough(cx, l)))
        .style(|s| s.absolute().size_full());
    stack((text, strikes)).style(|s| s.width_full())
}

/// A line through glyphs marked [`STRIKETHROUGH`], in the glyph's color.
fn paint_strikethrough(cx: &mut PaintCx, layout: &TextLayout) {
    for run in layout.layout_runs() {
        for g in run.glyphs.iter().filter(|g| g.metadata == STRIKETHROUGH) {
            let Some(c) = g.color_opt else { continue };
            let y = (run.line_y - g.font_size * 0.3).round() as f64;
            let rect = Rect::new(g.x as f64, y, (g.x + g.w) as f64, y + 1.0);
            cx.fill(&rect, Color::from_rgba8(c.r(), c.g(), c.b(), c.a()), 0.0);
        }
    }
}

/// The task checkbox (☐ or ☑) at a click, as its index among the checkboxes
/// drawn before it, from the rendered text before and after the click.
fn clicked_task(before: &str, after: &str) -> Option<usize> {
    let is_box = |c: char| c == '☐' || c == '☑';
    let before = if after.starts_with(is_box) {
        before
    } else {
        before.strip_suffix(is_box)?
    };
    Some(before.chars().filter(|&c| is_box(c)).count())
}

/// Flips the `n`th `[ ]` / `[x]` in `source`, which starts at `base` in the block.
fn toggle_task(ctx: &BlockCtx, source: &str, base: usize, n: usize) {
    let Some((checked, range)) = Parser::new_ext(source, markdown_options())
        .into_offset_iter()
        .filter_map(|(event, range)| match event {
            MdEvent::TaskListMarker(checked) => Some((checked, range)),
            _ => None,
        })
        .nth(n)
    else {
        return;
    };
    let start = block_start(ctx.blocks, ctx.index) + base + range.start;
    let end = block_start(ctx.blocks, ctx.index) + base + range.end;
    ctx.ed.doc().do_raw_edit(
        &[(Selection::region(start, end), if checked { "[ ]" } else { "[x]" })],
        EditType::Other,
    );
}

/// Ctrl+click (Cmd on macOS) on a link opens it: web links in the browser,
/// relative files in Planck, `#heading` anchors move the cursor. True if it did.
fn follow_link(ed: &EditorData, offset: usize, pe: &PointerInputEvent) -> bool {
    let modifier = if cfg!(target_os = "macos") {
        pe.modifiers.meta()
    } else {
        pe.modifiers.control()
    };
    if !modifier || pe.button.mouse_button() != MouseButton::Primary {
        return false;
    }
    let doc = ed.doc();
    let text = doc.buffer.with_untracked(|b| b.to_string());
    let Some(dest) = link_at(&text, offset) else {
        return false;
    };
    if let Some(anchor) = dest.strip_prefix('#') {
        if let Some(offset) = heading_offset(&text, anchor) {
            ed.cursor().update(|c| c.set_offset(offset, false, false));
        }
    } else if ["http://", "https://", "mailto:"].iter().any(|p| dest.starts_with(p)) {
        ed.common
            .internal_command
            .send(InternalCommand::OpenWebUri { uri: dest });
    } else if !dest.contains(':') {
        // shortcut: no percent-decoding (`my%20file.md`), add it if such links show up.
        let file = dest.split(['#', '?']).next().unwrap_or(&dest);
        let base = doc.content.with_untracked(|c| {
            c.path().and_then(|p| p.parent()).map(Path::to_path_buf)
        });
        if let Some(path) = base.map(|dir| dir.join(file)).filter(|p| p.is_file()) {
            ed.common
                .internal_command
                .send(InternalCommand::OpenFile { path });
        }
    }
    true
}

/// The destination of the link at `offset`, if any.
fn link_at(text: &str, offset: usize) -> Option<String> {
    Parser::new_ext(text, document_options())
        .into_offset_iter()
        .find_map(|(event, range)| match event {
            MdEvent::Start(Tag::Link { dest_url, .. })
                if range.start <= offset && offset < range.end =>
            {
                Some(dest_url.to_string())
            }
            _ => None,
        })
}

/// Where the heading with this anchor starts: its `{#id}`, else its GitHub-style slug.
fn heading_offset(text: &str, anchor: &str) -> Option<usize> {
    let mut heading: Option<(usize, String)> = None;
    for (event, range) in Parser::new_ext(text, document_options()).into_offset_iter() {
        match event {
            MdEvent::Start(Tag::Heading { id, .. }) => {
                if id.as_deref() == Some(anchor) {
                    return Some(range.start);
                }
                heading = Some((range.start, String::new()));
            }
            MdEvent::Text(t) | MdEvent::Code(t) => {
                if let Some((_, title)) = &mut heading {
                    title.push_str(&t);
                }
            }
            MdEvent::End(TagEnd::Heading(_)) => {
                if let Some((start, title)) = heading.take()
                    && slug(&title) == anchor.to_lowercase()
                {
                    return Some(start);
                }
            }
            _ => {}
        }
    }
    None
}

/// GitHub's heading anchor: lowercase, punctuation dropped, spaces as dashes.
/// shortcut: repeated headings don't get the `-1`, `-2` suffixes; the first one wins.
fn slug(title: &str) -> String {
    title
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

/// The line holding `offset`, without its newline.
fn line_range(text: &str, offset: usize) -> Range<usize> {
    let start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let end = text[offset..].find('\n').map_or(text.len(), |i| offset + i);
    start..end
}

/// Top-level markdown blocks covering the whole text, so every offset (blank
/// lines included) belongs to exactly one block.
fn split_blocks(text: &str) -> Vec<Block> {
    let mut starts: Vec<(usize, BlockKind)> = Vec::new();
    // Front matter only counts at the top of the file; pulldown-cmark would
    // also take a `---` / `key: value` / `---` run further down.
    let body = Parser::new_ext(text, document_options())
        .into_offset_iter()
        .next()
        .and_then(|(event, range)| {
            matches!(event, MdEvent::Start(Tag::MetadataBlock(_))).then_some(range.end)
        })
        .unwrap_or(0);
    if body > 0 {
        starts.push((0, BlockKind::FrontMatter));
    }
    let mut depth = 0usize;
    for (event, range) in
        Parser::new_ext(&text[body..], markdown_options()).into_offset_iter()
    {
        let range = range.start + body..range.end + body;
        match event {
            MdEvent::Start(tag) => {
                if depth == 0 {
                    let kind = match tag {
                        Tag::CodeBlock(_) => BlockKind::Code,
                        Tag::Table(_) => BlockKind::Table,
                        Tag::BlockQuote(kind) => BlockKind::Quote(kind),
                        _ => BlockKind::Other,
                    };
                    starts.push((range.start, kind));
                }
                depth += 1;
            }
            MdEvent::End(_) => depth = depth.saturating_sub(1),
            MdEvent::Rule if depth == 0 => starts.push((range.start, BlockKind::Rule)),
            _ if depth == 0 => starts.push((range.start, BlockKind::Other)),
            _ => {}
        }
    }

    let mut blocks: Vec<Block> = starts
        .iter()
        .enumerate()
        .map(|(i, (start, kind))| Block {
            range: if i == 0 { 0 } else { *start }
                ..starts.get(i + 1).map_or(text.len(), |(s, _)| *s),
            kind: *kind,
        })
        .collect();
    if blocks.is_empty() {
        blocks.push(Block {
            range: 0..text.len(),
            kind: BlockKind::Other,
        });
    }
    blocks
}

/// Table rows (header first) with each cell's text range, pipes and padding trimmed.
fn table_rows(text: &str) -> Vec<(bool, Vec<Range<usize>>)> {
    let mut rows: Vec<(bool, Vec<Range<usize>>)> = Vec::new();
    for (event, range) in Parser::new_ext(text, markdown_options()).into_offset_iter() {
        match event {
            MdEvent::Start(Tag::TableHead) => rows.push((true, Vec::new())),
            MdEvent::Start(Tag::TableRow) => rows.push((false, Vec::new())),
            MdEvent::Start(Tag::TableCell) => {
                if let Some((_, cells)) = rows.last_mut() {
                    let cell = &text[range.clone()];
                    let pad = |c: char| c.is_whitespace() || c == '|';
                    let start = range.start + cell.len() - cell.trim_start_matches(pad).len();
                    let end = range.end - (cell.len() - cell.trim_end_matches(pad).len());
                    cells.push(start..end.max(start));
                }
            }
            _ => {}
        }
    }
    rows
}

/// `key: value` rows of a YAML front matter block. Indented lines continue the
/// value above, keeping their source spelling.
fn front_matter_rows(text: &str) -> Vec<(Range<usize>, Range<usize>)> {
    let mut rows: Vec<(Range<usize>, Range<usize>)> = Vec::new();
    let mut offset = 0;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        let start = offset;
        offset += line.len();
        let content = line.trim_end_matches(['\n', '\r']);
        if i == 0 {
            continue;
        }
        if content == "---" || content == "..." {
            break;
        }
        let end = start + content.len();
        match content.find(':') {
            Some(colon) if !content.starts_with([' ', '\t', '-', '#']) => {
                let value = &content[colon + 1..];
                let value_start = end - value.trim_start().len();
                rows.push((start..start + content[..colon].trim_end().len(), value_start..end));
            }
            _ if !content.trim().is_empty() => {
                if let Some((_, value)) = rows.last_mut() {
                    if value.start == value.end {
                        value.start = start;
                    }
                    value.end = end;
                }
            }
            _ => {}
        }
    }
    rows
}

fn block_at(blocks: &[Block], offset: usize) -> usize {
    blocks
        .iter()
        .rposition(|b| b.range.start <= offset)
        .unwrap_or(0)
}

/// Maps rendered text back to its source: walks the source, skipping markup
/// characters that the rendered text does not have.
fn source_offset(source: &str, rendered_prefix: &str) -> usize {
    let mut pos = 0;
    for rc in rendered_prefix.chars() {
        let found = source[pos..]
            .char_indices()
            .find(|&(_, sc)| sc == rc || (rc == ' ' && sc == '\n'));
        // Characters only the rendering has (list bullets) are skipped.
        if let Some((i, sc)) = found {
            pos += i + sc.len_utf8();
        }
    }
    pos
}

fn rendered_text(layout: &TextLayout) -> String {
    layout
        .lines()
        .iter()
        .map(|l| l.text())
        .collect::<Vec<_>>()
        .join("\n")
}

fn hit_offset(layout: &TextLayout, pos: Point) -> Option<usize> {
    let hit = layout.hit(pos.x as f32, pos.y as f32)?;
    Some(layout.lines_range().get(hit.line)?.start + hit.index)
}

fn line_col(layout: &TextLayout, offset: usize) -> (usize, usize) {
    let ranges = layout.lines_range();
    for (i, r) in ranges.iter().enumerate() {
        if offset <= r.end {
            return (i, offset.saturating_sub(r.start));
        }
    }
    (ranges.len().saturating_sub(1), 0)
}

fn caret_rect(layout: &TextLayout, offset: usize, line_height: f64) -> Rect {
    let (line, col) = line_col(layout, offset);
    let mut end_of_line = None;
    for run in layout.layout_runs() {
        if run.line_i != line {
            continue;
        }
        let (top, height) = (run.line_top as f64, run.line_height as f64);
        if let Some(g) = run.glyphs.iter().find(|g| g.end > col) {
            let x = g.x as f64;
            return Rect::new(x, top, x + 2.0, top + height);
        }
        let x = run.glyphs.last().map_or(0.0, |g| (g.x + g.w) as f64);
        end_of_line = Some(Rect::new(x, top, x + 2.0, top + height));
    }
    end_of_line.unwrap_or(Rect::new(0.0, 0.0, 2.0, line_height))
}

fn paint_selection(
    cx: &mut PaintCx,
    layout: &TextLayout,
    selection: Range<usize>,
    color: Color,
) {
    let ranges = layout.lines_range();
    for run in layout.layout_runs() {
        let Some(line) = ranges.get(run.line_i) else {
            continue;
        };
        if selection.end < line.start || selection.start > line.end {
            continue;
        }
        let a = selection.start.max(line.start) - line.start;
        let b = selection.end.min(line.end) - line.start;
        let (mut x0, mut x1) = (f64::MAX, f64::MIN);
        for g in run.glyphs {
            if g.start >= a && g.end <= b {
                x0 = x0.min(g.x as f64);
                x1 = x1.max((g.x + g.w) as f64);
            }
        }
        // A selected newline shows as a small block after the line's last glyph.
        let line_ends_here = run
            .glyphs
            .last()
            .is_none_or(|g| g.end >= line.end - line.start);
        if selection.end > line.end && line_ends_here {
            let x = run.line_w as f64;
            x0 = x0.min(x);
            x1 = x1.max(x + 6.0);
        }
        if x0 < x1 {
            let top = run.line_top as f64;
            cx.fill(
                &Rect::new(x0, top, x1, top + run.line_height as f64),
                color,
                0.0,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_cover_the_whole_text() {
        let text = "\n# Title\n\nSome *text*\nmore\n\n---\n\n```rs\nfn x() {}\n```\n- a\n- b\n";
        let blocks = split_blocks(text);
        let kinds: Vec<_> = blocks.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            [
                BlockKind::Other,
                BlockKind::Other,
                BlockKind::Rule,
                BlockKind::Code,
                BlockKind::Other
            ]
        );
        assert_eq!(blocks[0].range.start, 0);
        assert_eq!(blocks.last().unwrap().range.end, text.len());
        for pair in blocks.windows(2) {
            assert_eq!(pair[0].range.end, pair[1].range.start);
        }
        assert_eq!(&text[blocks[1].range.clone()], "Some *text*\nmore\n\n");
        assert_eq!(block_at(&blocks, text.len()), blocks.len() - 1);
        assert_eq!(split_blocks("").len(), 1);
    }

    #[test]
    fn tables_and_front_matter_are_blocks() {
        let text = "---\ntitle: Notes\ntags:\n  - a\n  - b\n---\n# H\n\n| a | **b** |\n|---|:-:|\n| 1 |\n";
        let kinds: Vec<_> = split_blocks(text).iter().map(|b| b.kind).collect();
        assert_eq!(kinds, [BlockKind::FrontMatter, BlockKind::Other, BlockKind::Table]);

        let fm = &text[split_blocks(text)[0].range.clone()];
        let rows: Vec<_> = front_matter_rows(fm)
            .into_iter()
            .map(|(k, v)| (&fm[k], &fm[v]))
            .collect();
        assert_eq!(rows, [("title", "Notes"), ("tags", "  - a\n  - b")]);

        let table = &text[split_blocks(text)[2].range.clone()];
        let cells: Vec<Vec<&str>> = table_rows(table)
            .into_iter()
            .map(|(_, cells)| cells.into_iter().map(|r| &table[r]).collect())
            .collect();
        assert_eq!(cells, [vec!["a", "**b**"], vec!["1", ""]]);

        // A rule and key line later in the file are not front matter.
        let kinds: Vec<_> = split_blocks("text\n\n---\nfoo: bar\n---\n")
            .iter()
            .map(|b| b.kind)
            .collect();
        assert!(!kinds.contains(&BlockKind::FrontMatter), "{kinds:?}");
    }

    #[test]
    fn quotes_links_and_tasks() {
        let kinds: Vec<_> = split_blocks("> plain\n\n> [!WARNING]\n> careful\n")
            .iter()
            .map(|b| b.kind)
            .collect();
        assert_eq!(
            kinds,
            [BlockKind::Quote(None), BlockKind::Quote(Some(BlockQuoteKind::Warning))]
        );

        let text = "# Getting `Started`!\n\nSee [docs](other.md#x) and <https://a.b>.\n\n## Two {#custom}\n";
        assert_eq!(link_at(text, text.find("docs").unwrap()).as_deref(), Some("other.md#x"));
        assert_eq!(link_at(text, text.find("a.b").unwrap()).as_deref(), Some("https://a.b"));
        assert_eq!(link_at(text, text.find("See").unwrap()), None);
        assert_eq!(heading_offset(text, "getting-started"), Some(0));
        assert_eq!(heading_offset(text, "custom"), text.find("## Two"));
        assert_eq!(heading_offset(text, "nope"), None);

        assert_eq!(clicked_task("• ", "☐ a"), Some(0));
        assert_eq!(clicked_task("• ☑ a\n• ☐", " b"), Some(1));
        assert_eq!(clicked_task("• ☐ a", "b"), None);
    }

    #[test]
    fn svg_sizes() {
        assert_eq!(svg_size(r#"<svg xmlns="x" width="78" height="20">"#), Some((78.0, 20.0)));
        assert_eq!(svg_size(r#"<?xml?><svg viewBox="0 0 24 12" >"#), Some((24.0, 12.0)));
        assert_eq!(svg_size(r#"<svg width="10px" height='5'>"#), Some((10.0, 5.0)));
        assert_eq!(svg_size("<svg>"), None);
    }

    #[test]
    fn rendered_offsets_map_to_source() {
        assert_eq!(source_offset("# Title", "Ti"), 4);
        assert_eq!(source_offset("Hello **world**", "Hello w"), 9);
        assert_eq!(source_offset("- item", "• it"), 4);
        assert_eq!(source_offset("[link](http://x) end", "link e"), 18);
        assert_eq!(source_offset("one\ntwo", "one t"), 5);
    }
}
