use std::{rc::Rc, sync::Arc};

use floem::{
    View,
    event::EventListener,
    menu::{Menu, MenuItem},
    reactive::{Memo, ReadSignal, RwSignal, SignalGet, SignalUpdate, create_memo},
    style::{AlignItems, CursorStyle, JustifyContent},
    views::{
        Decorators, container, drag_window_area, empty, label, stack,
        stack_from_iter, svg,
    },
};

use crate::{
    app::{MENU_BAR, clickable_icon, menu_bar_menu, not_clickable_icon},
    command::{PlanckWorkbenchCommand, WindowCommand},
    config::{PlanckConfig, color::PlanckColor, icon::PlanckIcons},
    listener::Listener,
    main_split::MainSplitData,
    window_tab::WindowTabData,
    workspace::PlanckWorkspace,
};

fn left(
    data: Rc<WindowTabData>,
    workbench_command: Listener<PlanckWorkbenchCommand>,
    config: ReadSignal<Arc<PlanckConfig>>,
    num_window_tabs: Memo<usize>,
) -> impl View {
    let menus = MENU_BAR.map(|name| {
        let data = data.clone();
        label(move || name.to_string())
            .popout_menu(move || menu_bar_menu(&data, name))
            .style(move |s| {
                let config = config.get();
                s.padding_horiz(8.0)
                    .padding_vert(4.0)
                    .border_radius(6.0)
                    .hover(|s| {
                        s.cursor(CursorStyle::Pointer).background(
                            config.color(PlanckColor::PANEL_HOVERED_BACKGROUND),
                        )
                    })
                    .active(|s| {
                        s.background(
                            config
                                .color(PlanckColor::PANEL_HOVERED_ACTIVE_BACKGROUND),
                        )
                    })
            })
    });
    let is_macos = cfg!(target_os = "macos");
    stack((
        empty().style(move |s| {
            let should_hide = if is_macos {
                num_window_tabs.get() > 1
            } else {
                true
            };
            s.width(75.0).apply_if(should_hide, |s| s.hide())
        }),
        container(svg(move || config.get().ui_svg(PlanckIcons::LOGO)).style(
            move |s| {
                let config = config.get();
                s.size(16.0, 16.0)
                    .color(config.color(PlanckColor::PLANCK_ICON_ACTIVE))
            },
        ))
        .style(move |s| s.margin_horiz(10.0).apply_if(is_macos, |s| s.hide())),
        stack_from_iter(menus).style(|s| s.items_center().margin_right(4.0)),
        clickable_icon(
            || PlanckIcons::ADD,
            move || workbench_command.send(PlanckWorkbenchCommand::NewFile),
            || false,
            || false,
            || "New File",
            config,
        )
        .style(|s| s.margin_right(6.0)),
        drag_window_area(empty())
            .style(|s| s.height_pct(100.0).flex_basis(0.0).flex_grow(1.0f32)),
    ))
    .style(move |s| {
        s.height_pct(100.0)
            .flex_basis(0.0)
            .flex_grow(1.0f32)
            .items_center()
    })
    .debug_name("Left Side of Top Bar")
}

fn middle(
    workspace: Arc<PlanckWorkspace>,
    main_split: MainSplitData,
    workbench_command: Listener<PlanckWorkbenchCommand>,
    config: ReadSignal<Arc<PlanckConfig>>,
) -> impl View {
    let local_workspace = workspace.clone();
    let can_jump_backward = {
        let main_split = main_split.clone();
        create_memo(move |_| main_split.can_jump_location_backward(true))
    };
    let can_jump_forward =
        create_memo(move |_| main_split.can_jump_location_forward(true));

    let jump_backward = move || {
        clickable_icon(
            || PlanckIcons::LOCATION_BACKWARD,
            move || {
                workbench_command.send(PlanckWorkbenchCommand::JumpLocationBackward);
            },
            || false,
            move || !can_jump_backward.get(),
            || "Jump Backward",
            config,
        )
        .style(move |s| s.margin_horiz(6.0))
    };
    let jump_forward = move || {
        clickable_icon(
            || PlanckIcons::LOCATION_FORWARD,
            move || {
                workbench_command.send(PlanckWorkbenchCommand::JumpLocationForward);
            },
            || false,
            move || !can_jump_forward.get(),
            || "Jump Forward",
            config,
        )
        .style(move |s| s.margin_right(6.0))
    };

    let open_folder = move || {
        not_clickable_icon(
            || PlanckIcons::PALETTE_MENU,
            || false,
            || false,
            || "Open Folder, Space or Recent",
            config,
        )
        .popout_menu(move || {
            Menu::new("")
                .entry(MenuItem::new("Open Folder").action(move || {
                    workbench_command.send(PlanckWorkbenchCommand::OpenFolder);
                }))
                .entry(MenuItem::new("Open Space").action(move || {
                    workbench_command.send(PlanckWorkbenchCommand::OpenSpace);
                }))
                .entry(MenuItem::new("Open Recent").action(move || {
                    workbench_command.send(PlanckWorkbenchCommand::PaletteWorkspace);
                }))
        })
    };

    stack((
        stack((
            drag_window_area(empty())
                .style(|s| s.height_pct(100.0).flex_basis(0.0).flex_grow(1.0f32)),
            jump_backward(),
            jump_forward(),
        ))
        .style(|s| {
            s.flex_basis(0)
                .flex_grow(1.0f32)
                .justify_content(Some(JustifyContent::FlexEnd))
        }),
        container(
            stack((
                svg(move || config.get().ui_svg(PlanckIcons::SEARCH)).style(
                    move |s| {
                        let config = config.get();
                        let icon_size = config.ui.icon_size() as f32;
                        s.size(icon_size, icon_size)
                            .color(config.color(PlanckColor::PLANCK_ICON_ACTIVE))
                    },
                ),
                label(move || {
                    if let Some(s) = local_workspace.display() {
                        s
                    } else {
                        "Open Folder".to_string()
                    }
                })
                .style(|s| s.padding_left(10).padding_right(5).selectable(false)),
                open_folder(),
            ))
            .style(|s| s.align_items(Some(AlignItems::Center))),
        )
        .on_event_stop(EventListener::PointerDown, |_| {})
        .on_click_stop(move |_| {
            if workspace.clone().path.is_some() {
                workbench_command.send(PlanckWorkbenchCommand::PaletteHelpAndFile);
            } else {
                workbench_command.send(PlanckWorkbenchCommand::PaletteWorkspace);
            }
        })
        .style(move |s| {
            let config = config.get();
            s.flex_basis(0)
                .flex_grow(10.0f32)
                .min_width(200.0)
                .max_width(500.0)
                .height(26.0)
                .justify_content(Some(JustifyContent::Center))
                .align_items(Some(AlignItems::Center))
                .border(1.0)
                .border_color(config.color(PlanckColor::PLANCK_BORDER))
                .border_radius(6.0)
                .background(config.color(PlanckColor::EDITOR_BACKGROUND))
        }),
        stack((
            clickable_icon(
                || PlanckIcons::START,
                move || workbench_command.send(PlanckWorkbenchCommand::PaletteRun),
                || false,
                || false,
                || "Run and Debug",
                config,
            )
            .style(move |s| s.margin_horiz(6.0)),
            drag_window_area(empty())
                .style(|s| s.height_pct(100.0).flex_basis(0.0).flex_grow(1.0f32)),
        ))
        .style(move |s| {
            s.flex_basis(0)
                .flex_grow(1.0f32)
                .justify_content(Some(JustifyContent::FlexStart))
        }),
    ))
    .style(|s| {
        s.flex_basis(0)
            .flex_grow(2.0f32)
            .align_items(Some(AlignItems::Center))
            .justify_content(Some(JustifyContent::Center))
    })
    .debug_name("Middle of Top Bar")
}

fn right(
    window_command: Listener<WindowCommand>,
    num_window_tabs: Memo<usize>,
    window_maximized: RwSignal<bool>,
    config: ReadSignal<Arc<PlanckConfig>>,
) -> impl View {
    stack((
        drag_window_area(empty())
            .style(|s| s.height_pct(100.0).flex_basis(0.0).flex_grow(1.0f32)),
        window_controls_view(
            window_command,
            true,
            num_window_tabs,
            window_maximized,
            config,
        ),
    ))
    .style(|s| {
        s.flex_basis(0)
            .flex_grow(1.0f32)
            .justify_content(Some(JustifyContent::FlexEnd))
    })
    .debug_name("Right of top bar")
}

pub fn title(window_tab_data: Rc<WindowTabData>) -> impl View {
    let workspace = window_tab_data.workspace.clone();
    let workbench_command = window_tab_data.common.workbench_command;
    let window_command = window_tab_data.common.window_common.window_command;
    let num_window_tabs = window_tab_data.common.window_common.num_window_tabs;
    let window_maximized = window_tab_data.common.window_common.window_maximized;
    let title_height = window_tab_data.title_height;
    let config = window_tab_data.common.config;
    stack((
        left(
            window_tab_data.clone(),
            workbench_command,
            config,
            num_window_tabs,
        ),
        middle(
            workspace,
            window_tab_data.main_split.clone(),
            workbench_command,
            config,
        ),
        right(window_command, num_window_tabs, window_maximized, config),
    ))
    .on_resize(move |rect| {
        let height = rect.height();
        if height != title_height.get_untracked() {
            title_height.set(height);
        }
    })
    .style(move |s| {
        let config = config.get();
        s.width_pct(100.0)
            .height(37.0)
            .items_center()
            .background(config.color(PlanckColor::PANEL_BACKGROUND))
            .border_bottom(1.0)
            .border_color(config.color(PlanckColor::PLANCK_BORDER))
    })
    .debug_name("Title / Top Bar")
}

pub fn window_controls_view(
    window_command: Listener<WindowCommand>,
    is_title: bool,
    num_window_tabs: Memo<usize>,
    window_maximized: RwSignal<bool>,
    config: ReadSignal<Arc<PlanckConfig>>,
) -> impl View {
    stack((
        clickable_icon(
            || PlanckIcons::WINDOW_MINIMIZE,
            || {
                floem::action::minimize_window();
            },
            || false,
            || false,
            || "Minimize",
            config,
        )
        .style(|s| s.margin_right(16.0).margin_left(10.0)),
        clickable_icon(
            move || {
                if window_maximized.get() {
                    PlanckIcons::WINDOW_RESTORE
                } else {
                    PlanckIcons::WINDOW_MAXIMIZE
                }
            },
            move || {
                floem::action::set_window_maximized(
                    !window_maximized.get_untracked(),
                );
            },
            || false,
            || false,
            || "Maximize",
            config,
        )
        .style(|s| s.margin_right(16.0)),
        clickable_icon(
            || PlanckIcons::WINDOW_CLOSE,
            move || {
                window_command.send(WindowCommand::CloseWindow);
            },
            || false,
            || false,
            || "Close Window",
            config,
        )
        .style(|s| s.margin_right(6.0)),
    ))
    .style(move |s| {
        s.apply_if(
            cfg!(target_os = "macos")
                || !config.get_untracked().core.custom_titlebar
                || (is_title && num_window_tabs.get() > 1),
            |s| s.hide(),
        )
    })
}
