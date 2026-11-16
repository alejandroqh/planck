use std::{cell::RefCell, collections::HashMap, sync::Arc, time::SystemTime};

use alacritty_terminal::{
    grid::{Dimensions, Scroll},
    index::Side,
    selection::{Selection, SelectionType},
    term::{RenderableContent, TermMode, cell::Flags, test::TermSize},
};
use floem::{
    Renderer, View, ViewId,
    context::{EventCx, PaintCx},
    event::{Event, EventPropagation},
    keyboard::Modifiers,
    kurbo::Stroke,
    peniko::{
        Color,
        kurbo::{Point, Rect, Size},
    },
    pointer::PointerInputEvent,
    prelude::SignalTrack,
    reactive::{ReadSignal, RwSignal, SignalGet, SignalWith, create_effect},
    text::{Attrs, AttrsList, FamilyOwned, TextLayout, Weight},
    views::editor::{core::register::Clipboard, text::SystemClipboard},
};
use lsp_types::Position;
use parking_lot::RwLock;
use planck_core::mode::Mode;
use planck_rpc::{proxy::ProxyRpcHandler, terminal::TermId};
use regex::Regex;
use unicode_width::UnicodeWidthChar;

use super::{panel::TerminalPanelData, raw::RawTerminal};
use crate::{
    command::InternalCommand,
    config::{PlanckConfig, color::PlanckColor},
    editor::location::{EditorLocation, EditorPosition},
    listener::Listener,
    panel::kind::PanelKind,
    run::RunProcess,
    window_tab::Focus,
    workspace::PlanckWorkspace,
};

/// Threshold used for double_click/triple_click.
const CLICK_THRESHOLD: u128 = 400;

enum TerminalViewState {
    Config,
    Focus(bool),
    Raw(Arc<RwLock<RawTerminal>>),
}

struct TerminalLineContent<'a> {
    y: f64,
    bg: Vec<(usize, usize, Color)>,
    underline: Vec<(usize, usize, Color, f64)>,
    chars: Vec<(char, bool, Color, Attrs<'a>, f64, f64)>,
    cursor: Option<(char, f64)>,
}

pub struct TerminalView {
    id: ViewId,
    term_id: TermId,
    raw: Arc<RwLock<RawTerminal>>,
    mode: ReadSignal<Mode>,
    size: Size,
    is_focused: bool,
    config: ReadSignal<Arc<PlanckConfig>>,
    run_config: ReadSignal<Option<RunProcess>>,
    proxy: ProxyRpcHandler,
    launch_error: RwSignal<Option<String>>,
    internal_command: Listener<InternalCommand>,
    workspace: Arc<PlanckWorkspace>,
    hyper_regs: Vec<Regex>,
    previous_mouse_action: MouseAction,
    current_mouse_action: MouseAction,
    /// Button held down while the app gets mouse reports, and the last cell sent.
    report_button: Option<u8>,
    report_cell: (usize, usize),
    /// Shaped glyphs keyed by (char, bold, fg rgba), cleared on config change.
    glyph_cache: RefCell<HashMap<(char, bool, u32), TextLayout>>,
    /// Size of one cell, recomputed on config change.
    char_size: Size,
}

#[allow(clippy::too_many_arguments)]
pub fn terminal_view(
    term_id: TermId,
    raw: ReadSignal<Arc<RwLock<RawTerminal>>>,
    mode: ReadSignal<Mode>,
    run_config: ReadSignal<Option<RunProcess>>,
    terminal_panel_data: TerminalPanelData,
    launch_error: RwSignal<Option<String>>,
    internal_command: Listener<InternalCommand>,
    workspace: Arc<PlanckWorkspace>,
) -> TerminalView {
    let id = ViewId::new();

    create_effect(move |_| {
        let raw = raw.get();
        id.update_state(TerminalViewState::Raw(raw));
    });

    create_effect(move |_| {
        launch_error.track();
        id.request_paint();
    });

    let config = terminal_panel_data.common.config;
    create_effect(move |_| {
        config.with(|_c| {});
        id.update_state(TerminalViewState::Config);
    });

    let proxy = terminal_panel_data.common.proxy.clone();

    create_effect(move |last| {
        let focus = terminal_panel_data.common.focus.get();

        let mut is_focused = false;
        if let Focus::Panel(PanelKind::Terminal) = focus {
            let tab = terminal_panel_data.active_tab(true);
            if let Some(tab) = tab {
                let terminal = tab.active_terminal(true);
                is_focused = terminal.map(|t| t.term_id) == Some(term_id);
            }
        }

        if last != Some(is_focused) {
            id.update_state(TerminalViewState::Focus(is_focused));
        }

        is_focused
    });

    // for rust
    let reg = regex::Regex::new("[\\w\\\\/-]+\\.(rs)?(toml)?:\\d+(:\\d+)?").unwrap();

    TerminalView {
        id,
        term_id,
        raw: raw.get_untracked(),
        mode,
        config,
        proxy,
        run_config,
        size: Size::ZERO,
        is_focused: false,
        launch_error,
        internal_command,
        workspace,
        hyper_regs: vec![reg],
        previous_mouse_action: Default::default(),
        current_mouse_action: Default::default(),
        report_button: None,
        report_cell: (0, 0),
        glyph_cache: Default::default(),
        char_size: TerminalView::measure_char_size(&config.get_untracked()),
    }
}

impl TerminalView {
    fn char_size(&self) -> Size {
        self.char_size
    }

    fn measure_char_size(config: &PlanckConfig) -> Size {
        let font_family = config.terminal_font_family();
        let font_size = config.terminal_font_size();
        let family: Vec<FamilyOwned> =
            FamilyOwned::parse_list(font_family).collect();
        let attrs = Attrs::new().family(&family).font_size(font_size as f32);
        let attrs_list = AttrsList::new(attrs);
        let mut text_layout = TextLayout::new();
        text_layout.set_text("W", attrs_list, None);
        text_layout.size()
    }

    fn terminal_size(&self) -> (usize, usize) {
        let config = self.config.get_untracked();
        let line_height = config.terminal_line_height() as f64;
        let char_width = self.char_size().width;
        let width = (self.size.width / char_width).floor() as usize;
        let height = (self.size.height / line_height).floor() as usize;
        (width.max(1), height.max(1))
    }

    fn click(&self, pos: Point) -> Option<()> {
        let raw = self.raw.read();
        let position = self.get_terminal_point(pos);
        let start_point = raw.term.semantic_search_left(position);
        let end_point = raw.term.semantic_search_right(position);
        let mut selection =
            Selection::new(SelectionType::Simple, start_point, Side::Left);
        selection.update(end_point, Side::Right);
        selection.include_all();
        if let Some(selection) = selection.to_range(&raw.term) {
            let content = raw.term.bounds_to_string(selection.start, selection.end);
            if let Some(match_str) =
                self.hyper_regs.iter().find_map(|x| x.find(&content))
            {
                let hyperlink = match_str.as_str();
                let content: Vec<&str> = hyperlink.split(':').collect();
                let (file, line, col) = (
                    content.first()?,
                    content.get(1).and_then(|x: &&str| x.parse::<u32>().ok())?,
                    content
                        .get(2)
                        .and_then(|x: &&str| x.parse::<u32>().ok())
                        .unwrap_or(0),
                );
                let parent_path = self.workspace.path.as_ref()?;
                self.internal_command.send(InternalCommand::JumpToLocation {
                    location: EditorLocation {
                        path: parent_path.join(file),
                        position: Some(EditorPosition::Position(Position::new(
                            line.saturating_sub(1),
                            col.saturating_sub(1),
                        ))),
                        scroll_offset: None,
                        ignore_unconfirmed: false,
                        same_editor_tab: false,
                    },
                });
                return Some(());
            }
        }
        None
    }

    fn update_mouse_action_by_down(&mut self, mouse: &PointerInputEvent) {
        let mut next_action = MouseAction::None;
        match self.current_mouse_action {
            MouseAction::None
            | MouseAction::LeftDouble { .. }
            | MouseAction::LeftSelect { .. }
            | MouseAction::RightOnce { .. } => {
                if mouse.button.is_primary() {
                    next_action = MouseAction::LeftDown { pos: mouse.pos };
                } else if mouse.button.is_secondary() {
                    next_action = MouseAction::RightDown { pos: mouse.pos };
                }
            }
            MouseAction::LeftOnce { pos, time } => {
                let during_mills =
                    time.elapsed().map(|x| x.as_millis()).unwrap_or(u128::MAX);
                match (
                    mouse.button.is_primary(),
                    mouse.pos == pos,
                    during_mills < CLICK_THRESHOLD,
                ) {
                    (true, true, true) => {
                        next_action = MouseAction::LeftOnceAndDown { pos, time };
                    }
                    (true, _, _) => {
                        next_action = MouseAction::LeftDown { pos: mouse.pos };
                    }
                    _ => {}
                }
            }
            MouseAction::LeftOnceAndDown { .. }
            | MouseAction::LeftDown { .. }
            | MouseAction::RightDown { .. } => {}
        }
        self.current_mouse_action = next_action;
    }

    fn update_mouse_action_by_up(&mut self, mouse: &PointerInputEvent) {
        let mut next_action = MouseAction::None;
        match self.current_mouse_action {
            MouseAction::None => {}
            MouseAction::LeftDown { pos } => {
                match (mouse.button.is_primary(), mouse.pos == pos) {
                    (true, true) => {
                        next_action = MouseAction::LeftOnce {
                            pos,
                            time: SystemTime::now(),
                        }
                    }
                    (true, false) => {
                        next_action = MouseAction::LeftSelect {
                            start_pos: pos,
                            end_pos: mouse.pos,
                        };
                    }
                    _ => {}
                }
            }
            MouseAction::LeftOnce { .. } => {}
            MouseAction::LeftSelect { .. } => {}
            MouseAction::LeftOnceAndDown { pos, time } => {
                let during_mills =
                    time.elapsed().map(|x| x.as_millis()).unwrap_or(u128::MAX);
                match (
                    mouse.button.is_primary(),
                    mouse.pos == pos,
                    during_mills < CLICK_THRESHOLD,
                ) {
                    (true, true, true) => {
                        next_action = MouseAction::LeftDouble { pos };
                    }
                    (true, true, false) => {
                        next_action = MouseAction::LeftOnce {
                            pos: mouse.pos,
                            time: SystemTime::now(),
                        };
                    }
                    (true, false, _) => {
                        next_action = MouseAction::LeftSelect {
                            start_pos: pos,
                            end_pos: mouse.pos,
                        };
                    }
                    _ => {}
                }
            }
            MouseAction::LeftDouble { .. } => {}
            MouseAction::RightDown { pos } => {
                if mouse.button.is_secondary() && mouse.pos == pos {
                    next_action = MouseAction::RightOnce { pos };
                }
            }
            MouseAction::RightOnce { .. } => {}
        }
        self.previous_mouse_action = self.current_mouse_action;
        self.current_mouse_action = next_action;
    }

    /// The app's mouse mode when it wants clicks reported, unless Shift is held
    /// to select text instead, as in other terminals.
    fn mouse_mode(&self, modifiers: Modifiers) -> Option<TermMode> {
        if modifiers.shift() || self.mode.get_untracked() != Mode::Terminal {
            return None;
        }
        let mode = *self.raw.read().term.mode();
        mode.intersects(TermMode::MOUSE_MODE).then_some(mode)
    }

    /// The (column, row) cell under `pos` on screen, clamped to the grid.
    fn viewport_cell(&self, pos: Point) -> (usize, usize) {
        let (cols, rows) = self.terminal_size();
        let line_height = self.config.get_untracked().terminal_line_height() as f64;
        let col = (pos.x.max(0.0) / self.char_size().width) as usize;
        let row = (pos.y.max(0.0) / line_height) as usize;
        (col.min(cols - 1), row.min(rows - 1))
    }

    fn report_mouse(
        &self,
        mode: TermMode,
        code: u8,
        cell: (usize, usize),
        pressed: bool,
    ) {
        if let Some(msg) = mouse_report(mode, code, cell, pressed) {
            self.proxy.terminal_write(self.term_id, msg);
        }
    }

    /// Middle click: pastes the primary selection, or the clipboard where the
    /// platform has no primary selection (macOS, Windows).
    fn paste_selection(&self) {
        if self.mode.get_untracked() != Mode::Terminal {
            return;
        }
        let text = floem::Clipboard::get_selection()
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| SystemClipboard::new().get_string());
        if let Some(text) = text {
            let mut raw = self.raw.write();
            let text = raw.paste_text(&text);
            raw.term.scroll_display(Scroll::Bottom);
            self.proxy.terminal_write(self.term_id, text);
        }
    }

    fn get_terminal_point(&self, pos: Point) -> alacritty_terminal::index::Point {
        let raw = self.raw.read();
        let col = (pos.x / self.char_size().width) as usize;
        let line_no = pos.y as i32
            / (self.config.get().terminal_line_height() as i32)
            - raw.term.grid().display_offset() as i32;
        alacritty_terminal::index::Point::new(
            alacritty_terminal::index::Line(line_no),
            alacritty_terminal::index::Column(col),
        )
    }

    fn paint_content(
        &self,
        cx: &mut PaintCx,
        content: RenderableContent,
        line_height: f64,
        char_size: Size,
        config: &PlanckConfig,
    ) {
        let term_bg = config.color(PlanckColor::TERMINAL_BACKGROUND);

        let font_size = config.terminal_font_size();
        let font_family = config.terminal_font_family();
        let family: Vec<FamilyOwned> =
            FamilyOwned::parse_list(font_family).collect();
        let attrs = Attrs::new().family(&family).font_size(font_size as f32);

        let char_width = char_size.width;

        let cursor_point = &content.cursor.point;

        let mut line_content = TerminalLineContent {
            y: 0.0,
            bg: Vec::new(),
            underline: Vec::new(),
            chars: Vec::new(),
            cursor: None,
        };
        for item in content.display_iter {
            let point = item.point;
            let cell = item.cell;
            let inverse = cell.flags.contains(Flags::INVERSE);

            let x = point.column.0 as f64 * char_width;
            let y =
                (point.line.0 as f64 + content.display_offset as f64) * line_height;
            let char_y = y + (line_height - char_size.height) / 2.0;
            if y != line_content.y {
                self.paint_line_content(
                    cx,
                    &line_content,
                    line_height,
                    char_width,
                    config,
                );
                line_content.y = y;
                line_content.bg.clear();
                line_content.underline.clear();
                line_content.chars.clear();
                line_content.cursor = None;
            }

            let mut bg = config.terminal_get_color(&cell.bg, content.colors);
            let mut fg = config.terminal_get_color(&cell.fg, content.colors);
            if cell.flags.contains(Flags::DIM)
                || cell.flags.contains(Flags::DIM_BOLD)
            {
                fg = fg.multiply_alpha(0.66);
            }

            if inverse {
                std::mem::swap(&mut fg, &mut bg);
            }

            if term_bg != bg {
                let mut extend = false;
                if let Some((_, end, color)) = line_content.bg.last_mut() {
                    if color == &bg && *end == point.column.0 {
                        *end += 1;
                        extend = true;
                    }
                }
                if !extend {
                    line_content
                        .bg
                        .push((point.column.0, point.column.0 + 1, bg));
                }
            }

            if cursor_point == &point {
                line_content.cursor = Some((cell.c, x));
            }

            let bold = cell.flags.contains(Flags::BOLD)
                || cell.flags.contains(Flags::DIM_BOLD);

            if &point == cursor_point && self.is_focused {
                fg = term_bg;
            }

            if cell.c != ' ' && cell.c != '\t' {
                let mut attrs = attrs.clone().color(fg);
                if bold {
                    attrs = attrs.weight(Weight::BOLD);
                }
                line_content
                    .chars
                    .push((cell.c, bold, fg, attrs, x, char_y));
            }
        }
        self.paint_line_content(cx, &line_content, line_height, char_width, config);
    }

    fn paint_line_content(
        &self,
        cx: &mut PaintCx,
        line_content: &TerminalLineContent,
        line_height: f64,
        char_width: f64,
        config: &PlanckConfig,
    ) {
        for (start, end, bg) in &line_content.bg {
            let rect = Size::new(
                char_width * (end.saturating_sub(*start) as f64),
                line_height,
            )
            .to_rect()
            .with_origin(Point::new(*start as f64 * char_width, line_content.y));
            cx.fill(&rect, bg, 0.0);
        }

        for (start, end, fg, y) in &line_content.underline {
            let rect =
                Size::new(char_width * (end.saturating_sub(*start) as f64), 1.0)
                    .to_rect()
                    .with_origin(Point::new(*start as f64 * char_width, y - 1.0));
            cx.fill(&rect, fg, 0.0);
        }

        if let Some((c, x)) = line_content.cursor {
            let rect =
                Size::new(char_width * c.width().unwrap_or(1) as f64, line_height)
                    .to_rect()
                    .with_origin(Point::new(x, line_content.y));
            let mode = self.mode.get_untracked();
            let cursor_color = if mode == Mode::Terminal {
                if self.run_config.with_untracked(|run_config| {
                    run_config.as_ref().map(|r| r.stopped).unwrap_or(false)
                }) {
                    config.color(PlanckColor::PLANCK_ERROR)
                } else {
                    config.color(PlanckColor::TERMINAL_CURSOR)
                }
            } else {
                config.color(PlanckColor::EDITOR_CARET)
            };
            if self.is_focused {
                cx.fill(&rect, cursor_color, 0.0);
            } else {
                cx.stroke(&rect, cursor_color, &Stroke::new(1.0));
            }
        }

        let mut cache = self.glyph_cache.borrow_mut();
        // shortcut: unbounded growth is capped by a full clear, fine for terminal palettes
        if cache.len() > 8192 {
            cache.clear();
        }
        for (char, bold, fg, attr, x, y) in &line_content.chars {
            let text_layout = cache
                .entry((*char, *bold, fg.to_rgba8().to_u32()))
                .or_insert_with(|| {
                    let mut text_layout = TextLayout::new();
                    text_layout.set_text(
                        &char.to_string(),
                        AttrsList::new(attr.clone()),
                        None,
                    );
                    text_layout
                });
            cx.draw_text(text_layout, Point::new(*x, *y));
        }
    }
}

impl Drop for TerminalView {
    fn drop(&mut self) {
        self.proxy.terminal_close(self.term_id);
    }
}

impl View for TerminalView {
    fn id(&self) -> ViewId {
        self.id
    }

    fn event_before_children(
        &mut self,
        _cx: &mut EventCx,
        event: &Event,
    ) -> EventPropagation {
        match event {
            Event::PointerDown(e) => {
                // Continue in every case: the panel takes focus on pointer down.
                let button = if e.button.is_primary() {
                    Some(0)
                } else if e.button.is_auxiliary() {
                    Some(1)
                } else if e.button.is_secondary() {
                    Some(2)
                } else {
                    None
                };
                if let (Some(mode), Some(button)) =
                    (self.mouse_mode(e.modifiers), button)
                {
                    let cell = self.viewport_cell(e.pos);
                    let code = button + modifier_bits(e.modifiers);
                    self.report_mouse(mode, code, cell, true);
                    self.report_button = Some(button);
                    self.report_cell = cell;
                    _cx.update_active(self.id);
                    return EventPropagation::Continue;
                }
                if button == Some(1) {
                    self.paste_selection();
                    return EventPropagation::Continue;
                }
                self.update_mouse_action_by_down(e);
                if button == Some(0) {
                    // keep receiving moves while a selection is dragged outside
                    _cx.update_active(self.id);
                }
            }
            Event::PointerMove(e) => {
                if let Some(mode) = self.mouse_mode(e.modifiers) {
                    let cell = self.viewport_cell(e.pos);
                    let button = match self.report_button {
                        Some(button) => mode
                            .intersects(
                                TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION,
                            )
                            .then_some(button),
                        None => mode.contains(TermMode::MOUSE_MOTION).then_some(3),
                    };
                    if let (Some(button), true) = (button, cell != self.report_cell)
                    {
                        let code = 32 + button + modifier_bits(e.modifiers);
                        self.report_mouse(mode, code, cell, true);
                        self.report_cell = cell;
                    }
                    return EventPropagation::Continue;
                }
                if let MouseAction::LeftDown { pos }
                | MouseAction::LeftOnceAndDown { pos, .. } =
                    self.current_mouse_action
                {
                    let start = self.get_terminal_point(pos);
                    let end = self.get_terminal_point(e.pos);
                    if start != end {
                        let mut selection =
                            Selection::new(SelectionType::Simple, start, Side::Left);
                        selection.update(end, Side::Right);
                        selection.include_all();
                        self.raw.write().term.selection = Some(selection);
                        _cx.app_state_mut().request_paint(self.id);
                    }
                }
            }
            Event::PointerWheel(e) => {
                let report = self.mouse_mode(e.modifiers);
                let term_mode = *self.raw.read().term.mode();
                // less, man and other full-screen apps without mouse mode scroll by arrow keys
                let arrows = report.is_none()
                    && !e.modifiers.shift()
                    && self.mode.get_untracked() == Mode::Terminal
                    && term_mode
                        .contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL);
                if report.is_none() && !arrows {
                    // the panel's wheel listener scrolls the history
                    return EventPropagation::Continue;
                }
                let lines = {
                    let step =
                        self.config.get_untracked().terminal_line_height() as f64;
                    let mut raw = self.raw.write();
                    raw.scroll_delta -= e.delta.y;
                    let lines = (raw.scroll_delta / step) as i32;
                    raw.scroll_delta -= lines as f64 * step;
                    lines
                };
                let up = lines > 0;
                let cell = self.viewport_cell(e.pos);
                for _ in 0..lines.unsigned_abs() {
                    if let Some(mode) = report {
                        let code =
                            if up { 64 } else { 65 } + modifier_bits(e.modifiers);
                        self.report_mouse(mode, code, cell, true);
                    } else {
                        let app_cursor = term_mode.contains(TermMode::APP_CURSOR);
                        let seq = match (up, app_cursor) {
                            (true, true) => "\x1bOA",
                            (true, false) => "\x1b[A",
                            (false, true) => "\x1bOB",
                            (false, false) => "\x1b[B",
                        };
                        self.proxy.terminal_write(self.term_id, seq.to_string());
                    }
                }
                return EventPropagation::Stop;
            }
            Event::PointerUp(e) => {
                if let Some(button) = self.report_button.take() {
                    if let Some(mode) = self.mouse_mode(e.modifiers) {
                        let cell = self.viewport_cell(e.pos);
                        let code = button + modifier_bits(e.modifiers);
                        self.report_mouse(mode, code, cell, false);
                    }
                    // the app had the click, so no context menu or selection
                    return EventPropagation::Stop;
                }
                self.update_mouse_action_by_up(e);
                let mut clear_selection = false;
                match self.current_mouse_action {
                    MouseAction::LeftOnce { pos, .. } => {
                        clear_selection = true;
                        if e.modifiers.control() && self.click(pos).is_some() {
                            return EventPropagation::Stop;
                        }
                    }
                    MouseAction::LeftSelect { start_pos, end_pos } => {
                        let mut selection = Selection::new(
                            SelectionType::Simple,
                            self.get_terminal_point(start_pos),
                            Side::Left,
                        );
                        selection
                            .update(self.get_terminal_point(end_pos), Side::Right);
                        selection.include_all();
                        self.raw.write().term.selection = Some(selection);
                        _cx.app_state_mut().request_paint(self.id);
                    }
                    MouseAction::LeftDouble { pos } => {
                        let position = self.get_terminal_point(pos);
                        let mut raw = self.raw.write();
                        let start_point = raw.term.semantic_search_left(position);
                        let end_point = raw.term.semantic_search_right(position);

                        let mut selection = Selection::new(
                            SelectionType::Simple,
                            start_point,
                            Side::Left,
                        );
                        selection.update(end_point, Side::Right);
                        selection.include_all();
                        raw.term.selection = Some(selection);
                        _cx.app_state_mut().request_paint(self.id);
                    }
                    MouseAction::RightOnce { pos } => {
                        let position = self.get_terminal_point(pos);
                        let raw = self.raw.read();
                        if let Some(selection) = &raw
                            .term
                            .selection
                            .as_ref()
                            .and_then(|x| x.to_range(&raw.term))
                        {
                            if selection.contains(position) {
                                let mut clipboard = SystemClipboard::new();
                                let content = raw.term.bounds_to_string(
                                    selection.start,
                                    selection.end,
                                );
                                if !content.is_empty() {
                                    clipboard.put_string(content);
                                }
                            }
                            clear_selection = true;
                        }
                    }
                    _ => {
                        clear_selection = true;
                    }
                }
                if clear_selection {
                    self.raw.write().term.selection = None;
                    _cx.app_state_mut().request_paint(self.id);
                } else if let MouseAction::LeftSelect { .. }
                | MouseAction::LeftDouble { .. } =
                    self.current_mouse_action
                {
                    // Like other Linux terminals, a selection is ready for middle-click paste.
                    if let Some(text) = self.raw.read().term.selection_to_string() {
                        let _ = floem::Clipboard::set_selection(text);
                    }
                }
            }
            _ => {}
        }
        EventPropagation::Continue
    }

    fn update(
        &mut self,
        cx: &mut floem::context::UpdateCx,
        state: Box<dyn std::any::Any>,
    ) {
        if let Ok(state) = state.downcast() {
            match *state {
                TerminalViewState::Config => {
                    self.glyph_cache.borrow_mut().clear();
                    self.char_size =
                        Self::measure_char_size(&self.config.get_untracked());
                }
                TerminalViewState::Focus(is_focused) => {
                    self.is_focused = is_focused;
                }
                TerminalViewState::Raw(raw) => {
                    self.raw = raw;
                }
            }
            cx.app_state_mut().request_paint(self.id);
        }
    }

    fn layout(
        &mut self,
        cx: &mut floem::context::LayoutCx,
    ) -> floem::taffy::prelude::NodeId {
        cx.layout_node(self.id, false, |_cx| Vec::new())
    }

    fn compute_layout(
        &mut self,
        _cx: &mut floem::context::ComputeLayoutCx,
    ) -> Option<Rect> {
        let layout = self.id.get_layout().unwrap_or_default();
        let size = layout.size;
        let size = Size::new(size.width as f64, size.height as f64);
        if size.is_zero_area() {
            return None;
        }
        if size != self.size {
            self.size = size;
            let (width, height) = self.terminal_size();
            let term_size = TermSize::new(width, height);
            self.raw.write().term.resize(term_size);
            self.proxy.terminal_resize(self.term_id, width, height);
        }

        None
    }

    fn paint(&mut self, cx: &mut floem::context::PaintCx) {
        let config = self.config.get_untracked();
        let mode = self.mode.get_untracked();
        let line_height = config.terminal_line_height() as f64;
        let font_family = config.terminal_font_family();
        let font_size = config.terminal_font_size();
        let char_size = self.char_size();
        let char_width = char_size.width;

        let family: Vec<FamilyOwned> =
            FamilyOwned::parse_list(font_family).collect();
        let attrs = Attrs::new().family(&family).font_size(font_size as f32);

        if let Some(error) = self.launch_error.get() {
            let mut text_layout = TextLayout::new();
            text_layout.set_text(
                &format!("Terminal failed to launch. Error: {error}"),
                AttrsList::new(
                    attrs.color(config.color(PlanckColor::EDITOR_FOREGROUND)),
                ),
                None,
            );
            cx.draw_text(
                &text_layout,
                Point::new(6.0, 0.0 + (line_height - char_size.height) / 2.0),
            );
            return;
        }

        let raw = self.raw.read();
        let term = &raw.term;
        let content = term.renderable_content();

        // let mut search = RegexSearch::new("[\\w\\\\?]+\\.rs:\\d+:\\d+").unwrap();
        // self.hyper_matches = visible_regex_match_iter(term, &mut search).collect();

        if let Some(selection) = content.selection.as_ref() {
            let start_line = selection.start.line.0 + content.display_offset as i32;
            let start_line = if start_line < 0 {
                0
            } else {
                start_line as usize
            };
            let start_col = selection.start.column.0;

            let end_line = selection.end.line.0 + content.display_offset as i32;
            let end_line = if end_line < 0 { 0 } else { end_line as usize };
            let end_col = selection.end.column.0;

            for line in start_line..end_line + 1 {
                let left_col = if selection.is_block || line == start_line {
                    start_col
                } else {
                    0
                };
                let right_col = if selection.is_block || line == end_line {
                    end_col + 1
                } else {
                    term.last_column().0
                };
                let x0 = left_col as f64 * char_width;
                let x1 = right_col as f64 * char_width;
                let y0 = line as f64 * line_height;
                let y1 = y0 + line_height;
                cx.fill(
                    &Rect::new(x0, y0, x1, y1),
                    config.color(PlanckColor::EDITOR_SELECTION),
                    0.0,
                );
            }
        } else if mode != Mode::Terminal {
            let y = (content.cursor.point.line.0 as f64
                + content.display_offset as f64)
                * line_height;
            cx.fill(
                &Rect::new(0.0, y, self.size.width, y + line_height),
                config.color(PlanckColor::EDITOR_CURRENT_LINE),
                0.0,
            );
        }

        self.paint_content(cx, content, line_height, char_size, &config);
        // if data.find.visual {
        //     if let Some(search_string) = data.find.search_string.as_ref() {
        //         if let Ok(dfas) = RegexSearch::new(&regex::escape(search_string)) {
        //             let mut start = alacritty_terminal::index::Point::new(
        //                 alacritty_terminal::index::Line(
        //                     -(content.display_offset as i32),
        //                 ),
        //                 alacritty_terminal::index::Column(0),
        //             );
        //             let end_line = (start.line + term.screen_lines())
        //                 .min(term.bottommost_line());
        //             let mut max_lines = (end_line.0 - start.line.0) as usize;

        //             while let Some(m) = term.search_next(
        //                 &dfas,
        //                 start,
        //                 Direction::Right,
        //                 Side::Left,
        //                 Some(max_lines),
        //             ) {
        //                 let match_start = m.start();
        //                 if match_start.line.0 < start.line.0
        //                     || (match_start.line.0 == start.line.0
        //                         && match_start.column.0 < start.column.0)
        //                 {
        //                     break;
        //                 }
        //                 let x = match_start.column.0 as f64 * char_width;
        //                 let y = (match_start.line.0 as f64
        //                     + content.display_offset as f64)
        //                     * line_height;
        //                 let rect = Rect::ZERO
        //                     .with_origin(Point::new(x, y))
        //                     .with_size(Size::new(
        //                         (m.end().column.0 - m.start().column.0
        //                             + term.grid()[*m.end()].c.width().unwrap_or(1))
        //                             as f64
        //                             * char_width,
        //                         line_height,
        //                     ));
        //                 cx.stroke(
        //                     &rect,
        //                     config.get_color(PlanckColor::TERMINAL_FOREGROUND),
        //                     1.0,
        //                 );
        //                 start = *m.end();
        //                 if start.column.0 < term.last_column() {
        //                     start.column.0 += 1;
        //                 } else if start.line.0 < term.bottommost_line() {
        //                     start.column.0 = 0;
        //                     start.line.0 += 1;
        //                 }
        //                 max_lines = (end_line.0 - start.line.0) as usize;
        //             }
        //         }
        //     }
        // }
    }
}

/// A mouse event for the app at `cell` (column, row from 0), in the encoding
/// it asked for: SGR, else X10 (UTF-8 coordinates with UTF8_MOUSE).
fn mouse_report(
    mode: TermMode,
    code: u8,
    cell: (usize, usize),
    pressed: bool,
) -> Option<String> {
    let (col, row) = (cell.0 + 1, cell.1 + 1);
    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if pressed { 'M' } else { 'm' };
        return Some(format!("\x1b[<{code};{col};{row}{end}"));
    }
    // shortcut: plain X10 mode drops clicks past column/row 95, since
    // terminal_write takes a String; upgrade if an app without SGR needs them.
    if !mode.contains(TermMode::UTF8_MOUSE) && (col > 95 || row > 95) {
        return None;
    }
    // X10 releases don't say which button
    let code = if pressed { code } else { (code & !3) | 3 };
    let ch = |v: usize| char::from_u32(32 + v as u32).unwrap_or(' ');
    Some(format!("\x1b[M{}{}{}", ch(code as usize), ch(col), ch(row)))
}

/// Alt and Ctrl as the mouse-report encoding adds them to the button code.
fn modifier_bits(modifiers: Modifiers) -> u8 {
    (if modifiers.alt() { 8 } else { 0 })
        + (if modifiers.control() { 16 } else { 0 })
}

#[derive(Debug, Default, Copy, Clone)]
enum MouseAction {
    #[default]
    None,
    LeftDown {
        pos: Point,
    },
    LeftOnce {
        pos: Point,
        time: SystemTime,
    },
    LeftSelect {
        start_pos: Point,
        end_pos: Point,
    },
    LeftOnceAndDown {
        pos: Point,
        time: SystemTime,
    },
    LeftDouble {
        pos: Point,
    },
    RightDown {
        pos: Point,
    },
    RightOnce {
        pos: Point,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_report_encodings() {
        let sgr = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(mouse_report(sgr, 0, (0, 0), true).unwrap(), "\x1b[<0;1;1M");
        assert_eq!(
            mouse_report(sgr, 2, (9, 4), false).unwrap(),
            "\x1b[<2;10;5m"
        );

        let x10 = TermMode::MOUSE_REPORT_CLICK;
        assert_eq!(mouse_report(x10, 0, (2, 3), true).unwrap(), "\x1b[M #$");
        // a release reports button 3, keeping the modifier bits
        assert_eq!(
            mouse_report(x10, 16 + 2, (2, 3), false).unwrap(),
            "\x1b[M3#$"
        );
        assert_eq!(mouse_report(x10, 0, (100, 0), true), None);

        let utf8 = x10 | TermMode::UTF8_MOUSE;
        assert_eq!(
            mouse_report(utf8, 0, (100, 0), true).unwrap(),
            "\x1b[M \u{85}!"
        );
    }
}
