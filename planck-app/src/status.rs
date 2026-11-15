use std::{
    collections::VecDeque,
    path::Path,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::channel,
    },
    time::Duration,
};

use floem::{
    IntoView, Renderer, View,
    event::EventPropagation,
    ext_event::create_signal_from_channel,
    peniko::{
        Color,
        kurbo::{BezPath, Point, Stroke},
    },
    reactive::{
        Memo, ReadSignal, RwSignal, SignalGet, SignalUpdate, SignalWith, create_memo,
    },
    style::{AlignItems, CursorStyle, Display},
    views::{
        Decorators, canvas, clip, dyn_container, dyn_stack, empty, label, stack, svg,
    },
};
use indexmap::IndexMap;
use lsp_types::{DiagnosticSeverity, ProgressToken};
use planck_core::mode::{Mode, VisualMode};
use sysinfo::{Disks, System};

use crate::{
    app::clickable_icon,
    command::PlanckWorkbenchCommand,
    config::{PlanckConfig, color::PlanckColor, icon::PlanckIcons},
    editor::EditorData,
    listener::Listener,
    palette::kind::PaletteKind,
    panel::{kind::PanelKind, position::PanelContainerPosition},
    source_control::SourceControlData,
    window_tab::{WindowTabData, WorkProgress},
};

pub fn status(
    window_tab_data: Rc<WindowTabData>,
    source_control: SourceControlData,
    workbench_command: Listener<PlanckWorkbenchCommand>,
    status_height: RwSignal<f64>,
    _config: ReadSignal<Arc<PlanckConfig>>,
) -> impl View {
    let config = window_tab_data.common.config;
    let diagnostics = window_tab_data.main_split.diagnostics;
    let editor = window_tab_data.main_split.active_editor;
    let panel = window_tab_data.panel.clone();
    let palette = window_tab_data.palette.clone();
    let diagnostic_count = create_memo(move |_| {
        let mut errors = 0;
        let mut warnings = 0;
        for (_, diagnostics) in diagnostics.get().iter() {
            for diagnostic in diagnostics.diagnostics.get().iter() {
                if let Some(severity) = diagnostic.severity {
                    match severity {
                        DiagnosticSeverity::ERROR => errors += 1,
                        DiagnosticSeverity::WARNING => warnings += 1,
                        _ => (),
                    }
                }
            }
        }
        (errors, warnings)
    });
    let branch = source_control.branch;
    let file_diffs = source_control.file_diffs;
    let branch = move || {
        format!(
            "{}{}",
            branch.get(),
            if file_diffs.with(|diffs| diffs.is_empty()) {
                ""
            } else {
                "*"
            }
        )
    };

    let progresses = window_tab_data.progresses;
    let mode = create_memo(move |_| window_tab_data.mode());
    let pointer_down = floem::reactive::create_rw_signal(false);

    stack((
        // Clipped so a narrow window cuts the left items instead of painting them over the toggles.
        clip(
            stack((
                label(move || match mode.get() {
                    Mode::Normal => "Normal".to_string(),
                    Mode::Insert => "Insert".to_string(),
                    Mode::Visual(mode) => match mode {
                        VisualMode::Normal => "Visual".to_string(),
                        VisualMode::Linewise => "Visual Line".to_string(),
                        VisualMode::Blockwise => "Visual Block".to_string(),
                    },
                    Mode::Terminal => "Terminal".to_string(),
                })
                .style(move |s| {
                    let config = config.get();
                    let display = if config.core.modal {
                        Display::Flex
                    } else {
                        Display::None
                    };

                    let (bg, fg) = match mode.get() {
                        Mode::Normal => (
                            PlanckColor::STATUS_MODAL_NORMAL_BACKGROUND,
                            PlanckColor::STATUS_MODAL_NORMAL_FOREGROUND,
                        ),
                        Mode::Insert => (
                            PlanckColor::STATUS_MODAL_INSERT_BACKGROUND,
                            PlanckColor::STATUS_MODAL_INSERT_FOREGROUND,
                        ),
                        Mode::Visual(_) => (
                            PlanckColor::STATUS_MODAL_VISUAL_BACKGROUND,
                            PlanckColor::STATUS_MODAL_VISUAL_FOREGROUND,
                        ),
                        Mode::Terminal => (
                            PlanckColor::STATUS_MODAL_TERMINAL_BACKGROUND,
                            PlanckColor::STATUS_MODAL_TERMINAL_FOREGROUND,
                        ),
                    };

                    let bg = config.color(bg);
                    let fg = config.color(fg);

                    s.display(display)
                        .padding_horiz(10.0)
                        .color(fg)
                        .background(bg)
                        .height_pct(100.0)
                        .align_items(Some(AlignItems::Center))
                        .selectable(false)
                }),
                stack((
                    svg(move || config.get().ui_svg(PlanckIcons::SCM)).style(
                        move |s| {
                            let config = config.get();
                            let icon_size = config.ui.icon_size() as f32;
                            s.size(icon_size, icon_size)
                                .color(config.color(PlanckColor::PLANCK_ICON_ACTIVE))
                        },
                    ),
                    label(branch).style(move |s| {
                        s.margin_left(10.0)
                            .color(
                                config.get().color(PlanckColor::STATUS_FOREGROUND),
                            )
                            .selectable(false)
                    }),
                ))
                .style(move |s| {
                    s.display(if branch().is_empty() {
                        Display::None
                    } else {
                        Display::Flex
                    })
                    .height_pct(100.0)
                    .padding_horiz(10.0)
                    .align_items(Some(AlignItems::Center))
                    .hover(|s| {
                        s.cursor(CursorStyle::Pointer).background(
                            config
                                .get()
                                .color(PlanckColor::PANEL_HOVERED_BACKGROUND),
                        )
                    })
                })
                .on_event_cont(floem::event::EventListener::PointerDown, move |_| {
                    pointer_down.set(true);
                })
                .on_event(
                    floem::event::EventListener::PointerUp,
                    move |_| {
                        if pointer_down.get() {
                            workbench_command
                                .send(PlanckWorkbenchCommand::PaletteSCMReferences);
                        }
                        pointer_down.set(false);
                        EventPropagation::Continue
                    },
                ),
                {
                    let panel = panel.clone();
                    stack((
                        svg(move || config.get().ui_svg(PlanckIcons::ERROR)).style(
                            move |s| {
                                let config = config.get();
                                let size = config.ui.icon_size() as f32;
                                s.size(size, size).color(
                                    config.color(PlanckColor::PLANCK_ICON_ACTIVE),
                                )
                            },
                        ),
                        label(move || diagnostic_count.get().0.to_string()).style(
                            move |s| {
                                s.margin_left(5.0)
                                    .color(
                                        config
                                            .get()
                                            .color(PlanckColor::STATUS_FOREGROUND),
                                    )
                                    .selectable(false)
                            },
                        ),
                        svg(move || config.get().ui_svg(PlanckIcons::WARNING))
                            .style(move |s| {
                                let config = config.get();
                                let size = config.ui.icon_size() as f32;
                                s.size(size, size).margin_left(5.0).color(
                                    config.color(PlanckColor::PLANCK_ICON_ACTIVE),
                                )
                            }),
                        label(move || diagnostic_count.get().1.to_string()).style(
                            move |s| {
                                s.margin_left(5.0)
                                    .color(
                                        config
                                            .get()
                                            .color(PlanckColor::STATUS_FOREGROUND),
                                    )
                                    .selectable(false)
                            },
                        ),
                    ))
                    .on_click_stop(move |_| {
                        panel.show_panel(&PanelKind::Problem);
                    })
                    .style(move |s| {
                        s.height_pct(100.0)
                            .padding_horiz(10.0)
                            .items_center()
                            .hover(|s| {
                                s.cursor(CursorStyle::Pointer).background(
                                    config.get().color(
                                        PlanckColor::PANEL_HOVERED_BACKGROUND,
                                    ),
                                )
                            })
                    })
                },
                {
                    let enabled =
                        create_memo(move |_| config.get().ui.system_monitor);
                    dyn_container(
                        move || enabled.get(),
                        move |enabled| {
                            if enabled {
                                sys_stats_view(config).into_any()
                            } else {
                                empty().into_any()
                            }
                        },
                    )
                    .style(|s| s.height_pct(100.0))
                },
                progress_view(config, progresses),
            ))
            .style(|s| s.size_full().min_width(0.0).items_center()),
        )
        .style(|s| {
            s.height_pct(100.0)
                .min_width(0.0)
                .flex_basis(0.0)
                .flex_grow(1.0f32)
        }),
        stack((
            {
                let panel = panel.clone();
                let icon = {
                    let panel = panel.clone();
                    move || {
                        if panel
                            .is_container_shown(&PanelContainerPosition::Left, true)
                        {
                            PlanckIcons::SIDEBAR_LEFT
                        } else {
                            PlanckIcons::SIDEBAR_LEFT_OFF
                        }
                    }
                };
                clickable_icon(
                    icon,
                    move || {
                        panel.toggle_container_visual(&PanelContainerPosition::Left)
                    },
                    || false,
                    || false,
                    || "Toggle Left Panel",
                    config,
                )
            },
            {
                let panel = panel.clone();
                let icon = {
                    let panel = panel.clone();
                    move || {
                        if panel.is_container_shown(
                            &PanelContainerPosition::Bottom,
                            true,
                        ) {
                            PlanckIcons::LAYOUT_PANEL
                        } else {
                            PlanckIcons::LAYOUT_PANEL_OFF
                        }
                    }
                };
                clickable_icon(
                    icon,
                    move || {
                        panel
                            .toggle_container_visual(&PanelContainerPosition::Bottom)
                    },
                    || false,
                    || false,
                    || "Toggle Bottom Panel",
                    config,
                )
            },
            {
                let panel = panel.clone();
                let icon = {
                    let panel = panel.clone();
                    move || {
                        if panel
                            .is_container_shown(&PanelContainerPosition::Right, true)
                        {
                            PlanckIcons::SIDEBAR_RIGHT
                        } else {
                            PlanckIcons::SIDEBAR_RIGHT_OFF
                        }
                    }
                };
                clickable_icon(
                    icon,
                    move || {
                        panel.toggle_container_visual(&PanelContainerPosition::Right)
                    },
                    || false,
                    || false,
                    || "Toggle Right Panel",
                    config,
                )
            },
        ))
        .style(move |s| {
            s.height_pct(100.0)
                .items_center()
                .color(config.get().color(PlanckColor::STATUS_FOREGROUND))
        }),
        stack({
            let palette_clone = palette.clone();
            let cursor_info = status_text(config, editor, move || {
                if let Some(editor) = editor.get() {
                    let mut status = String::new();
                    let cursor = editor.cursor().get();
                    if let Some((line, column, character)) = editor
                        .doc_signal()
                        .get()
                        .buffer
                        .with(|buffer| cursor.get_line_col_char(buffer))
                    {
                        status = format!(
                            "Ln {}, Col {}, Char {}",
                            line + 1,
                            column + 1,
                            character,
                        );
                    }
                    if let Some(selection) = cursor.get_selection() {
                        let selection_range = selection.0.abs_diff(selection.1);

                        if selection.0 != selection.1 {
                            status =
                                format!("{status} ({selection_range} selected)");
                        }
                    }
                    let selection_count = cursor.get_selection_count();
                    if selection_count > 1 {
                        status = format!("{status} {selection_count} selections");
                    }
                    return status;
                }
                String::new()
            })
            .on_click_stop(move |_| {
                palette_clone.run(PaletteKind::Line);
            });
            let palette_clone = palette.clone();
            let line_ending_info = status_text(config, editor, move || {
                if let Some(editor) = editor.get() {
                    let doc = editor.doc_signal().get();
                    doc.buffer.with(|b| b.line_ending()).as_str()
                } else {
                    ""
                }
            })
            .on_click_stop(move |_| {
                palette_clone.run(PaletteKind::LineEnding);
            });
            let palette_clone = palette.clone();
            let language_info = status_text(config, editor, move || {
                if let Some(editor) = editor.get() {
                    let doc = editor.doc_signal().get();
                    doc.syntax().with(|s| s.language.name())
                } else {
                    "unknown"
                }
            })
            .on_click_stop(move |_| {
                palette_clone.run(PaletteKind::Language);
            });
            (cursor_info, line_ending_info, language_info)
        })
        .style(|s| {
            s.height_pct(100.0)
                .flex_basis(0.0)
                .flex_grow(1.0f32)
                .justify_end()
        }),
    ))
    .on_resize(move |rect| {
        let height = rect.height();
        if height != status_height.get_untracked() {
            status_height.set(height);
        }
    })
    .style(move |s| {
        let config = config.get();
        s.border_top(1.0)
            .border_color(config.color(PlanckColor::PLANCK_BORDER))
            .background(config.color(PlanckColor::STATUS_BACKGROUND))
            .flex_basis(config.ui.status_height() as f32)
            .flex_grow(0.0f32)
            .flex_shrink(0.0f32)
            .items_center()
    })
    .debug_name("Status/Bottom Bar")
}

/// CPU samples kept for the status bar graph, one per second.
const CPU_SAMPLES: usize = 60;

#[derive(Clone, Default)]
struct SysStats {
    /// Total CPU usage in percent, oldest first.
    cpu: VecDeque<f64>,
    /// Used and total bytes of memory.
    memory: (u64, u64),
    /// Used and total bytes of the disk mounted at `/`, if there is one.
    disk: Option<(u64, u64)>,
}

/// Samples every second on its own thread until `stop` is set.
fn sample_sys_stats(stop: Arc<AtomicBool>) -> ReadSignal<Option<SysStats>> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("SysStats".to_owned())
        .spawn(move || {
            let Ok(mut sys) = System::new() else { return };
            let mut disks = Disks::new_with_refreshed_list().ok();
            let root = disks.as_ref().and_then(|disks| {
                disks.iter().position(|d| d.mount_point() == Path::new("/"))
            });
            let mut stats = SysStats::default();
            sys.refresh_cpu_usage();
            loop {
                std::thread::sleep(Duration::from_secs(1));
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                sys.refresh_cpu_usage();
                sys.refresh_memory();
                if stats.cpu.len() == CPU_SAMPLES {
                    stats.cpu.pop_front();
                }
                stats.cpu.push_back(sys.global_cpu_usage() as f64);
                // Total minus available, so caches the system can drop count as free.
                let total = sys.total_memory();
                stats.memory = (total.saturating_sub(sys.available_memory()), total);
                stats.disk = root.zip(disks.as_mut()).map(|(i, disks)| {
                    let disk = &mut disks[i];
                    disk.refresh();
                    let total = disk.total_space();
                    (total.saturating_sub(disk.available_space()), total)
                });
                if tx.send(stats.clone()).is_err() {
                    break;
                }
            }
        })
        .unwrap();
    create_signal_from_channel(rx)
}

fn format_bytes(bytes: u64) -> String {
    let gib = bytes as f64 / (1u64 << 30) as f64;
    if gib >= 1000.0 {
        format!("{:.1}T", gib / 1024.0)
    } else {
        format!("{gib:.1}G")
    }
}

/// Used and free, "8.1G/23.2G".
fn used_free((used, total): (u64, u64)) -> String {
    format!(
        "{}/{}",
        format_bytes(used),
        format_bytes(total.saturating_sub(used))
    )
}

/// A base color of the theme, such as "magenta", which every theme inherits from the default.
fn base_color(config: &PlanckConfig, name: &str) -> Color {
    config
        .color
        .base
        .get(name)
        .unwrap_or_else(|| config.color(PlanckColor::STATUS_FOREGROUND))
}

fn stat_icon(
    config: ReadSignal<Arc<PlanckConfig>>,
    icon: &'static str,
    color: &'static str,
) -> impl View {
    svg(move || config.get().ui_svg(icon)).style(move |s| {
        let config = config.get();
        let size = config.ui.icon_size() as f32;
        s.size(size, size)
            .margin_left(10.0)
            .color(base_color(&config, color))
    })
}

fn stat_label(
    config: ReadSignal<Arc<PlanckConfig>>,
    color: &'static str,
    text: impl Fn() -> String + 'static,
) -> impl View {
    label(text).style(move |s| {
        s.margin_left(5.0)
            .color(base_color(&config.get(), color))
            .selectable(false)
    })
}

/// Total CPU graph and usage, then used and free memory and `/` disk.
fn sys_stats_view(config: ReadSignal<Arc<PlanckConfig>>) -> impl View {
    let stop = Arc::new(AtomicBool::new(false));
    let stats = sample_sys_stats(stop.clone());
    let cpu = move || {
        stats
            .with(|s| s.as_ref().and_then(|s| s.cpu.back().copied()))
            .unwrap_or(0.0)
    };
    let memory = move || {
        stats
            .with(|s| s.as_ref().map(|s| s.memory))
            .unwrap_or_default()
    };
    let disk = move || stats.with(|s| s.as_ref().and_then(|s| s.disk));

    stack((
        stat_icon(config, PlanckIcons::STATUS_CPU, "magenta"),
        canvas(move |cx, size| {
            let config = config.get();
            stats.with(|s| {
                let Some(s) = s else { return };
                // Newest sample on the right edge, one step per second.
                let step = size.width / (CPU_SAMPLES - 1) as f64;
                let x0 = size.width - step * s.cpu.len().saturating_sub(1) as f64;
                let mut line = BezPath::new();
                for (i, v) in s.cpu.iter().enumerate() {
                    let point = Point::new(
                        x0 + step * i as f64,
                        size.height * (1.0 - v.clamp(0.0, 100.0) / 100.0),
                    );
                    if i == 0 {
                        line.move_to(point);
                    } else {
                        line.line_to(point);
                    }
                }
                let color = base_color(&config, "magenta");
                let mut area = line.clone();
                area.line_to((size.width, size.height));
                area.line_to((x0, size.height));
                area.close_path();
                cx.fill(&area, color.multiply_alpha(0.25), 0.0);
                cx.stroke(&line, color, &Stroke::new(1.0));
            });
        })
        .style(|s| s.width(150.0).height_pct(60.0).margin_left(6.0)),
        stat_label(config, "magenta", move || format!("{:.0}%", cpu()))
            .style(|s| s.min_width(32.0)),
        stat_icon(config, PlanckIcons::STATUS_MEMORY, "orange"),
        stat_label(config, "orange", move || used_free(memory())),
        stack((
            stat_icon(config, PlanckIcons::STATUS_DISK, "blue"),
            stat_label(config, "blue", move || {
                disk().map(used_free).unwrap_or_default()
            }),
        ))
        .style(move |s| {
            s.height_pct(100.0)
                .items_center()
                .apply_if(disk().is_none(), |s| s.hide())
        }),
    ))
    .style(move |s| {
        s.height_pct(100.0)
            .padding_right(10.0)
            .items_center()
            .flex_shrink(0.0f32)
            .apply_if(stats.with(|s| s.is_none()), |s| s.hide())
    })
    .on_cleanup(move || stop.store(true, Ordering::Relaxed))
    .debug_name("System Stats")
}

fn progress_view(
    config: ReadSignal<Arc<PlanckConfig>>,
    progresses: RwSignal<IndexMap<ProgressToken, WorkProgress>>,
) -> impl View {
    dyn_stack(
        move || progresses.get(),
        // key on what is shown so unchanged items keep their view across updates
        |(token, p)| (token.clone(), p.title.clone(), p.message.clone()),
        move |(_, p)| {
            let progress = match p.message {
                Some(message) if !message.is_empty() => {
                    format!("{}: {}", p.title, message)
                }
                _ => p.title,
            };
            label(move || progress.clone()).style(move |s| {
                s.height_pct(100.0)
                    .min_width(0.0)
                    .margin_left(10.0)
                    .text_ellipsis()
                    .selectable(false)
                    .items_center()
                    .color(config.get().color(PlanckColor::STATUS_FOREGROUND))
            })
        },
    )
    .style(move |s| s.flex_row().height_pct(100.0).min_width(0.0))
}

fn status_text<S: std::fmt::Display + 'static>(
    config: ReadSignal<Arc<PlanckConfig>>,
    editor: Memo<Option<EditorData>>,
    text: impl Fn() -> S + 'static,
) -> impl View {
    label(text).style(move |s| {
        let config = config.get();
        let display = if editor
            .get()
            .map(|editor| {
                editor.doc_signal().get().content.with(|c| {
                    use crate::doc::DocContent;
                    matches!(c, DocContent::File { .. } | DocContent::Scratch { .. })
                })
            })
            .unwrap_or(false)
        {
            Display::Flex
        } else {
            Display::None
        };

        s.display(display)
            .height_full()
            .padding_horiz(10.0)
            .items_center()
            .color(config.color(PlanckColor::STATUS_FOREGROUND))
            .hover(|s| {
                s.cursor(CursorStyle::Pointer)
                    .background(config.color(PlanckColor::PANEL_HOVERED_BACKGROUND))
            })
            .selectable(false)
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn used_free_switches_to_terabytes() {
        assert_eq!(super::used_free((8 << 30, 32 << 30)), "8.0G/24.0G");
        assert_eq!(super::used_free((1 << 40, 2 << 40)), "1.0T/1.0T");
    }
}
