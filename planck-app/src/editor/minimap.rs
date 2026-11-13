//! A minimap: a scaled-down overview of the whole document drawn at the right
//! edge of the editor, with a box marking the visible part.
//!
//! Lines are drawn as small blocks colored by syntax highlighting rather than
//! as real text, which keeps painting cheap even for large files.

use floem::{
    Renderer, View, ViewId,
    context::{EventCx, PaintCx},
    event::{Event, EventPropagation},
    peniko::{
        Color,
        kurbo::{Rect, Size, Vec2},
    },
    reactive::{SignalGet, SignalUpdate, SignalWith, create_effect},
};
use planck_core::buffer::rope_text::RopeText;

use super::EditorData;
use crate::config::color::PlanckColor;

/// Height of one document line in the minimap.
const LINE_HEIGHT: f64 = 3.0;
/// Height of the blocks drawn for a line's text.
const BLOCK_HEIGHT: f64 = 2.0;
/// Width of one character column.
const CHAR_WIDTH: f64 = 1.5;
/// Gap between the left edge of the minimap and the text.
const PADDING_LEFT: f64 = 4.0;

pub struct MinimapView {
    id: ViewId,
    editor: EditorData,
    /// While dragging the viewport box: the pointer's offset from the box top.
    drag_offset: Option<f64>,
    hovered: bool,
}

/// Where things are, in minimap coordinates.
struct Geometry {
    total_lines: usize,
    /// First and number of document lines visible in the editor.
    first_line: usize,
    visible_lines: usize,
    /// How far the minimap itself is scrolled, for documents taller than it.
    scroll: f64,
    height: f64,
}

impl Geometry {
    fn content_height(&self) -> f64 {
        self.total_lines as f64 * LINE_HEIGHT
    }

    /// The box marking the part of the document visible in the editor.
    fn viewport_box(&self, width: f64) -> Rect {
        let y0 = self.first_line as f64 * LINE_HEIGHT - self.scroll;
        let y1 = y0 + self.visible_lines as f64 * LINE_HEIGHT;
        Rect::new(0.0, y0, width, y1)
    }

    /// The first editor line that puts the viewport box top at `top`.
    ///
    /// When the document is taller than the minimap, the box travels from the
    /// top to the bottom of the minimap as the editor scrolls through the
    /// whole document.
    fn first_line_for_box_top(&self, top: f64) -> usize {
        let line = if self.content_height() <= self.height {
            top / LINE_HEIGHT
        } else {
            let box_height = self.visible_lines as f64 * LINE_HEIGHT;
            let travel = self.height - box_height;
            let max_first = self.total_lines.saturating_sub(self.visible_lines);
            if travel <= 0.0 {
                0.0
            } else {
                top.clamp(0.0, travel) / travel * max_first as f64
            }
        };
        (line.max(0.0).round() as usize).min(self.total_lines.saturating_sub(1))
    }
}

pub fn minimap_view(editor: EditorData) -> MinimapView {
    let id = ViewId::new();

    // Repaint when the document, its styling, the scroll position or the
    // theme changes.
    let doc = editor.doc_signal();
    let screen_lines = editor.screen_lines();
    let config = editor.common.config;
    create_effect(move |_| {
        let doc = doc.get();
        doc.buffer.with(|b| b.rev());
        doc.cache_rev.get();
        doc.syntax().with(|_| ());
        screen_lines.with(|_| ());
        config.with(|_| ());
        id.request_paint();
    });

    MinimapView {
        id,
        editor,
        drag_offset: None,
        hovered: false,
    }
}

impl MinimapView {
    fn size(&self) -> Size {
        self.id.get_size().unwrap_or_default()
    }

    fn geometry(&self) -> Geometry {
        let height = self.size().height;
        let total_lines = self
            .editor
            .doc()
            .buffer
            .with_untracked(|b| b.num_lines())
            .max(1);
        let (first_line, last_line) =
            self.editor.screen_lines().with_untracked(|sl| {
                (
                    sl.lines.first().map(|l| l.line).unwrap_or(0),
                    sl.lines.last().map(|l| l.line).unwrap_or(0),
                )
            });
        let visible_lines = (last_line.saturating_sub(first_line) + 1).max(1);

        let content_height = total_lines as f64 * LINE_HEIGHT;
        let scroll = if content_height <= height {
            0.0
        } else {
            let max_first = total_lines.saturating_sub(visible_lines).max(1);
            let progress = (first_line as f64 / max_first as f64).min(1.0);
            progress * (content_height - height)
        };

        Geometry {
            total_lines,
            first_line,
            visible_lines,
            scroll,
            height,
        }
    }

    /// Scroll the editor so that `line` is the first visible line.
    fn scroll_editor_to_line(&self, line: usize) {
        let config = self.editor.common.config.get_untracked();
        let line_height = config.editor.line_height() as f64;
        let vline = self.editor.editor.vline_of_line(line).get();
        let y = self.editor.visual_line(vline) as f64 * line_height;
        let x = self.editor.viewport().get_untracked().x0;
        self.editor.scroll_to().set(Some(Vec2::new(x, y)));
    }

    fn drag_to(&self, pointer_y: f64) {
        if let Some(offset) = self.drag_offset {
            let geometry = self.geometry();
            let line = geometry.first_line_for_box_top(pointer_y - offset);
            self.scroll_editor_to_line(line);
        }
    }
}

impl View for MinimapView {
    fn id(&self) -> ViewId {
        self.id
    }

    fn debug_name(&self) -> std::borrow::Cow<'static, str> {
        "Minimap".into()
    }

    fn event_before_children(
        &mut self,
        cx: &mut EventCx,
        event: &Event,
    ) -> EventPropagation {
        match event {
            Event::PointerDown(pointer) => {
                let geometry = self.geometry();
                let viewport_box = geometry.viewport_box(self.size().width);
                let y = pointer.pos.y;
                // Grab the box where it was clicked, or center it on the click.
                let offset = if viewport_box.y0 <= y && y <= viewport_box.y1 {
                    y - viewport_box.y0
                } else {
                    viewport_box.height() / 2.0
                };
                self.drag_offset = Some(offset);
                cx.update_active(self.id);
                self.drag_to(y);
                self.id.request_paint();
                EventPropagation::Stop
            }
            Event::PointerMove(pointer) => {
                if !self.hovered {
                    self.hovered = true;
                    self.id.request_paint();
                }
                self.drag_to(pointer.pos.y);
                EventPropagation::Stop
            }
            Event::PointerUp(_) => {
                self.drag_offset = None;
                self.id.request_paint();
                EventPropagation::Stop
            }
            Event::PointerLeave => {
                self.hovered = false;
                self.id.request_paint();
                EventPropagation::Continue
            }
            Event::PointerWheel(wheel) => {
                self.editor.scroll_delta().set(wheel.delta);
                EventPropagation::Stop
            }
            _ => EventPropagation::Continue,
        }
    }

    fn paint(&mut self, cx: &mut PaintCx) {
        let size = self.size();
        if size.width <= 0.0 || size.height <= 0.0 {
            return;
        }
        let config = self.editor.common.config.get_untracked();
        let geometry = self.geometry();
        let doc = self.editor.doc();

        cx.save();
        cx.clip(&size.to_rect());

        let first = (geometry.scroll / LINE_HEIGHT).floor() as usize;
        let last = (((geometry.scroll + size.height) / LINE_HEIGHT).ceil() as usize)
            .min(geometry.total_lines);
        let max_cols = ((size.width - PADDING_LEFT) / CHAR_WIDTH).max(0.0) as usize;
        let tab_width = config.editor.tab_width.max(1);
        let default_color = config
            .color(PlanckColor::EDITOR_FOREGROUND)
            .multiply_alpha(0.5);

        let lines: Vec<String> = doc.buffer.with_untracked(|buffer| {
            (first..last)
                .map(|line| buffer.line_content(line).into_owned())
                .collect()
        });

        for (i, text) in lines.iter().enumerate() {
            let line = first + i;
            let y = line as f64 * LINE_HEIGHT - geometry.scroll;
            let styles = doc.line_style(line);
            let mut style_iter = styles.iter().peekable();

            // Merge consecutive characters of the same color into one block.
            let mut run: Option<(usize, usize, Color)> = None;
            let mut col = 0;
            let flush =
                |cx: &mut PaintCx, run: &mut Option<(usize, usize, Color)>| {
                    if let Some((start, end, color)) = run.take() {
                        let rect = Rect::new(
                            PADDING_LEFT + start as f64 * CHAR_WIDTH,
                            y,
                            PADDING_LEFT + end as f64 * CHAR_WIDTH,
                            y + BLOCK_HEIGHT,
                        );
                        cx.fill(&rect, color, 0.0);
                    }
                };

            for (byte, ch) in text.char_indices() {
                if col >= max_cols {
                    break;
                }
                if ch == '\t' {
                    flush(cx, &mut run);
                    col += tab_width - col % tab_width;
                    continue;
                }
                if ch.is_whitespace() {
                    flush(cx, &mut run);
                    col += 1;
                    continue;
                }

                while style_iter.peek().is_some_and(|s| s.end <= byte) {
                    style_iter.next();
                }
                let color = style_iter
                    .peek()
                    .filter(|s| s.start <= byte)
                    .and_then(|s| s.style.fg_color.as_deref())
                    .and_then(|name| config.style_color(name))
                    .map(|c| c.multiply_alpha(0.8))
                    .unwrap_or(default_color);

                match &mut run {
                    Some((_, end, run_color)) if *run_color == color => {
                        *end = col + 1;
                    }
                    _ => {
                        flush(cx, &mut run);
                        run = Some((col, col + 1, color));
                    }
                }
                col += 1;
            }
            flush(cx, &mut run);
        }

        let viewport_box = geometry.viewport_box(size.width);
        // Muted text color, so the viewport box stands apart from the gray scroll bar.
        let slider_color = config.color(PlanckColor::EDITOR_FOREGROUND);
        let slider_color = if self.hovered || self.drag_offset.is_some() {
            slider_color.multiply_alpha(0.2)
        } else {
            slider_color.multiply_alpha(0.1)
        };
        cx.fill(&viewport_box, slider_color, 0.0);

        cx.restore();
    }
}
