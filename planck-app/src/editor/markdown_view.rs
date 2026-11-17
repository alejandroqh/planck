//! The Rich view of a markdown file: the same document and cursor as the text
//! editor, drawn as formatted blocks. The block holding the cursor shows its raw
//! markdown so it is edited in place, and keys go to the editor as usual.

use std::{cell::Cell, ops::Range, sync::Arc};

use floem::{
    IntoView, Renderer, View,
    context::PaintCx,
    event::{Event, EventListener, EventPropagation},
    peniko::{
        Color,
        kurbo::{Point, Rect},
    },
    pointer::{MouseButton, PointerInputEvent},
    reactive::{
        Memo, RwSignal, SignalGet, SignalUpdate, SignalWith, create_effect,
        create_memo, create_rw_signal,
    },
    style::CursorStyle,
    text::TextLayout,
    views::{
        Decorators, canvas, dyn_stack, empty, rich_text, scroll, stack,
        stack_from_iter,
    },
};
use planck_core::cursor::CursorMode;
use pulldown_cmark::{Event as MdEvent, Parser, Tag};

use super::EditorData;
use crate::{
    command::InternalCommand,
    config::color::PlanckColor,
    markdown::{
        MarkdownContent, markdown_options, parse_markdown, style_markdown_source,
    },
    window_tab::Focus,
};

/// Line spacing of rendered blocks, and of the block being edited so it doesn't jump.
const RENDERED_LINE_HEIGHT: f64 = 1.6;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum BlockKind {
    Rule,
    Code,
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
                    rendered_block(ed, blocks, item, revealed).into_any()
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

/// A formatted block. Clicking it puts the cursor at the matching spot in the source.
fn rendered_block(
    ed: EditorData,
    blocks: Memo<Vec<Block>>,
    item: BlockItem,
    revealed: RwSignal<bool>,
) -> impl View {
    let config = ed.common.config.get_untracked();
    let source = Arc::new(item.text);
    let index = item.index;
    let content = if item.kind == BlockKind::Rule {
        vec![MarkdownContent::Separator]
    } else {
        parse_markdown(&source, RENDERED_LINE_HEIGHT, &config)
    };

    let border = config.color(PlanckColor::PLANCK_BORDER);
    let mut rendered_before = String::new();
    let mut pieces = Vec::new();
    for piece in content {
        let view = match piece {
            MarkdownContent::Text(layout) => {
                let prefix = rendered_before.clone();
                rendered_before.push_str(&rendered_text(&layout));
                rendered_before.push('\n');
                let layout = create_rw_signal(layout);
                let ed = ed.clone();
                let source = source.clone();
                wrapped_text(layout)
                    .on_event_stop(EventListener::PointerDown, move |event| {
                        let Event::PointerDown(pe) = event else { return };
                        let Some(hit) = layout.with_untracked(|l| {
                            let hit = hit_offset(l, pe.pos)?;
                            let rendered = rendered_text(l);
                            let before = rendered.get(..hit).unwrap_or(&rendered);
                            Some(format!("{prefix}{before}"))
                        }) else {
                            return;
                        };
                        let offset = block_start(blocks, index)
                            + source_offset(&source, &hit);
                        revealed.set(true);
                        click(&ed, offset, pe);
                    })
                    .into_any()
            }
            MarkdownContent::Separator => empty()
                .style(move |s| {
                    s.width_full()
                        .margin_vert(8.0)
                        .height(1.0)
                        .background(border)
                })
                .into_any(),
            // shortcut: images are not drawn yet, load them from the path or URL when asked for.
            MarkdownContent::Image { .. } => empty().into_any(),
        };
        pieces.push(view);
    }
    let line_height = config.editor.line_height() as f32;
    let code_background = config.color(PlanckColor::EDITOR_CURRENT_LINE);
    let is_code = item.kind == BlockKind::Code;

    stack_from_iter(pieces)
        .on_event_stop(EventListener::PointerDown, move |event| {
            if let Event::PointerDown(pe) = event {
                revealed.set(true);
                click(&ed, block_start(blocks, index), pe);
            }
        })
        .style(move |s| {
            s.flex_col()
                .width_full()
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
    let line_height = config.editor.line_height() as f64;
    // The last block can end in a newline with the cursor after it: give that line a glyph.
    let shown = if item.last && (item.text.is_empty() || item.text.ends_with('\n'))
    {
        format!("{} ", item.text)
    } else {
        item.text
    };
    let layout = create_rw_signal(TextLayout::new_with_text(
        &shown,
        style_markdown_source(&shown, RENDERED_LINE_HEIGHT, &config),
        None,
    ));

    let caret_color = config.color(PlanckColor::EDITOR_CARET);
    let selection_color = config.color(PlanckColor::EDITOR_SELECTION);
    let cursor = ed.cursor();
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

    let view = stack((paint, wrapped_text(layout)));
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
            click(&ed, offset_at(pe.pos), pe);
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
    .style(move |s| s.width_full().min_height(line_height as f32).padding_vert(4.0))
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
fn wrapped_text(layout: RwSignal<TextLayout>) -> impl View {
    let width = Cell::new(-1.0);
    rich_text(move || layout.get())
        .on_resize(move |rect| {
            if rect.width() != width.get() {
                width.set(rect.width());
                layout.update(|l| l.set_size(rect.width() as f32, f32::MAX));
            }
        })
        .style(|s| s.width_full())
}

/// Top-level markdown blocks covering the whole text, so every offset (blank
/// lines included) belongs to exactly one block.
fn split_blocks(text: &str) -> Vec<Block> {
    let mut starts: Vec<(usize, BlockKind)> = Vec::new();
    let mut depth = 0usize;
    for (event, range) in Parser::new_ext(text, markdown_options()).into_offset_iter()
    {
        match event {
            MdEvent::Start(tag) => {
                if depth == 0 {
                    let kind = if matches!(tag, Tag::CodeBlock(_)) {
                        BlockKind::Code
                    } else {
                        BlockKind::Other
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
    fn rendered_offsets_map_to_source() {
        assert_eq!(source_offset("# Title", "Ti"), 4);
        assert_eq!(source_offset("Hello **world**", "Hello w"), 9);
        assert_eq!(source_offset("- item", "• it"), 4);
        assert_eq!(source_offset("[link](http://x) end", "link e"), 18);
        assert_eq!(source_offset("one\ntwo", "one t"), 5);
    }
}
