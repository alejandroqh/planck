use std::{
    fmt,
    rc::Rc,
    sync::{Arc, atomic::AtomicU64},
};

use floem::{
    View,
    event::EventListener,
    reactive::{ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate},
    style::{CursorStyle, Style},
    views::{Decorators, container, dyn_stack, label, stack, svg},
};

use crate::{
    config::{PlanckConfig, color::PlanckColor, icon::PlanckIcons},
    window_tab::CommonData,
};

#[derive(Clone)]
pub struct AlertButton {
    pub text: String,
    /// Theme color key for the button border.
    pub color: &'static str,
    pub action: Rc<dyn Fn()>,
}

impl fmt::Debug for AlertButton {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = f.debug_struct("AlertButton");
        s.field("text", &self.text);
        s.finish()
    }
}

#[derive(Clone)]
pub struct AlertBoxData {
    pub active: RwSignal<bool>,
    pub title: RwSignal<String>,
    pub msg: RwSignal<String>,
    pub buttons: RwSignal<Vec<AlertButton>>,
    pub config: ReadSignal<Arc<PlanckConfig>>,
}

impl AlertBoxData {
    pub fn new(cx: Scope, common: Rc<CommonData>) -> Self {
        Self {
            active: cx.create_rw_signal(false),
            title: cx.create_rw_signal("".to_string()),
            msg: cx.create_rw_signal("".to_string()),
            buttons: cx.create_rw_signal(Vec::new()),
            config: common.config,
        }
    }
}

pub fn alert_box(alert_data: AlertBoxData) -> impl View {
    let config = alert_data.config;
    let active = alert_data.active;
    let title = alert_data.title;
    let msg = alert_data.msg;
    let buttons = alert_data.buttons;
    let button_id = AtomicU64::new(0);

    let button_style = move |color: &'static str| {
        move |s: Style| {
            let config = config.get();
            s.padding_horiz(16.0)
                .font_size(config.ui.font_size() as f32)
                .line_height(1.8)
                .border(1.0)
                .border_radius(6.0)
                .border_color(config.color(color))
                .hover(|s| {
                    s.cursor(CursorStyle::Pointer).background(
                        config.color(PlanckColor::PANEL_HOVERED_BACKGROUND),
                    )
                })
                .active(|s| {
                    s.background(
                        config.color(PlanckColor::PANEL_HOVERED_ACTIVE_BACKGROUND),
                    )
                })
        }
    };

    container(
        container({
            stack((
                stack((
                    svg(move || config.get().ui_svg(PlanckIcons::WARNING)).style(
                        move |s| {
                            s.size(32.0, 32.0)
                                .flex_shrink(0.0_f32)
                                .margin_right(16.0)
                                .color(config.get().color(PlanckColor::PLANCK_WARN))
                        },
                    ),
                    stack((
                        label(move || title.get()).style(move |s| {
                            s.width_pct(100.0)
                                .font_bold()
                                .font_size((config.get().ui.font_size() + 1) as f32)
                        }),
                        label(move || msg.get()).style(move |s| {
                            s.width_pct(100.0)
                                .margin_top(8.0)
                                .color(config.get().color(PlanckColor::EDITOR_DIM))
                        }),
                    ))
                    .style(|s| s.flex_col().flex_grow(1.0_f32).flex_basis(0.0)),
                ))
                .style(|s| s.items_start().width_pct(100.0)),
                stack((
                    label(|| "Cancel".to_string())
                        .on_click_stop(move |_| {
                            active.set(false);
                        })
                        .style(button_style(PlanckColor::PLANCK_BORDER)),
                    dyn_stack(
                        move || buttons.get(),
                        move |_button| {
                            button_id
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                        },
                        move |button| {
                            let color = button.color;
                            label(move || button.text.clone())
                                .on_click_stop(move |_| {
                                    (button.action)();
                                })
                                .style(button_style(color))
                        },
                    )
                    .style(|s| s.gap(8.0)),
                ))
                .style(|s| {
                    s.justify_end().gap(8.0).margin_top(24.0).width_pct(100.0)
                }),
            ))
            .style(|s| s.flex_col().width_pct(100.0))
        })
        .on_event_stop(EventListener::PointerDown, |_| {})
        .style(move |s| {
            let config = config.get();
            s.padding(20.0)
                .width(440.0)
                .border(1.0)
                .border_radius(8.0)
                .border_color(config.color(PlanckColor::PLANCK_BORDER))
                .color(config.color(PlanckColor::EDITOR_FOREGROUND))
                .background(config.color(PlanckColor::PANEL_BACKGROUND))
        }),
    )
    .on_event_stop(EventListener::PointerDown, move |_| {})
    .style(move |s| {
        s.absolute()
            .size_pct(100.0, 100.0)
            .items_center()
            .justify_center()
            .apply_if(!active.get(), |s| s.hide())
            .background(
                config
                    .get()
                    .color(PlanckColor::PLANCK_DROPDOWN_SHADOW)
                    .multiply_alpha(0.5),
            )
    })
    .debug_name("Alert Box")
}
