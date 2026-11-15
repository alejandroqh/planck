use std::{rc::Rc, sync::Arc};

use floem::{
    Renderer, View,
    ext_event::create_ext_action,
    kurbo::{BezPath, Circle, Line, Point, Size, Stroke},
    peniko::Color,
    reactive::{
        Memo, ReadSignal, SignalGet, SignalTrack, SignalUpdate, create_effect,
    },
    text::Weight,
    views::{Decorators, canvas, label, scroll, stack, virtual_stack},
};
use planck_rpc::{proxy::ProxyResponse, source_control::GraphCommit};

use crate::{
    config::{PlanckConfig, color::PlanckColor},
    editor::view::time_ago,
    window_tab::WindowTabData,
};

const LANE_WIDTH: f64 = 14.0;
/// Theme base colors the lanes cycle through.
const LANE_COLORS: [&str; 7] = [
    "blue", "green", "yellow", "magenta", "orange", "red", "cyan",
];

/// A line drawn inside one row, in a lane's column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Edge {
    /// A lane passing by, top to bottom.
    Pass(usize),
    /// A lane from the top that ends at this row's commit.
    In(usize),
    /// A lane from this row's commit down to one of its parents.
    Out(usize),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Row {
    commit: GraphCommit,
    /// Lane of the commit's dot.
    column: usize,
    color: usize,
    /// Every line of the row with its color.
    edges: Vec<(Edge, usize)>,
}

impl Row {
    fn width(&self) -> usize {
        self.edges
            .iter()
            .map(|(edge, _)| match edge {
                Edge::Pass(i) | Edge::In(i) | Edge::Out(i) => i + 1,
            })
            .max()
            .unwrap_or(0)
            .max(self.column + 1)
    }
}

/// Lanes like `git log --graph`: each lane waits for one commit, and a commit's
/// first parent keeps its lane. `commits` must list children before parents.
fn layout(commits: Vec<GraphCommit>) -> Vec<Row> {
    // The commit each lane waits for, and the lane's color.
    let mut lanes: Vec<Option<(String, usize)>> = Vec::new();
    let mut colors = 0..;
    commits
        .into_iter()
        .map(|commit| {
            let waits_for = |lane: &Option<(String, usize)>, id: &str| {
                lane.as_ref().is_some_and(|(lane_id, _)| lane_id == id)
            };
            let mut edges = Vec::new();
            for (i, lane) in lanes.iter().enumerate() {
                if let Some((id, color)) = lane {
                    let edge = if *id == commit.info.id {
                        Edge::In(i)
                    } else {
                        Edge::Pass(i)
                    };
                    edges.push((edge, *color));
                }
            }
            let (column, color) =
                match lanes.iter().position(|l| waits_for(l, &commit.info.id)) {
                    Some(i) => (i, lanes[i].as_ref().unwrap().1),
                    // A branch tip: no child waits for it.
                    None => (free_lane(&mut lanes), colors.next().unwrap()),
                };
            for lane in lanes.iter_mut() {
                if waits_for(lane, &commit.info.id) {
                    *lane = None;
                }
            }
            for (n, parent) in commit.parents.iter().enumerate() {
                let (lane, lane_color) =
                    match lanes.iter().position(|l| waits_for(l, parent)) {
                        _ if n == 0 => (column, color),
                        Some(j) => (j, lanes[j].as_ref().unwrap().1),
                        None => (free_lane(&mut lanes), colors.next().unwrap()),
                    };
                lanes[lane] = Some((parent.clone(), lane_color));
                edges.push((Edge::Out(lane), lane_color));
            }
            while lanes.last().is_some_and(Option::is_none) {
                lanes.pop();
            }
            Row {
                commit,
                column,
                color,
                edges,
            }
        })
        .collect()
}

fn free_lane(lanes: &mut Vec<Option<(String, usize)>>) -> usize {
    lanes.iter().position(Option::is_none).unwrap_or_else(|| {
        lanes.push(None);
        lanes.len() - 1
    })
}

fn lane_color(config: &PlanckConfig, color: usize) -> Color {
    config
        .color
        .base
        .get(LANE_COLORS[color % LANE_COLORS.len()])
        .unwrap_or_else(|| config.color(PlanckColor::EDITOR_FOREGROUND))
}

fn paint_row(
    cx: &mut floem::context::PaintCx,
    size: Size,
    row: &Row,
    config: &PlanckConfig,
) {
    let x = |lane: usize| LANE_WIDTH * (lane as f64 + 0.5);
    let dot = Point::new(x(row.column), size.height / 2.0);
    let stroke = Stroke::new(1.5);
    for &(edge, color) in &row.edges {
        let (from, to) = match edge {
            Edge::Pass(i) => (Point::new(x(i), 0.0), Point::new(x(i), size.height)),
            Edge::In(i) => (Point::new(x(i), 0.0), dot),
            Edge::Out(i) => (dot, Point::new(x(i), size.height)),
        };
        let color = lane_color(config, color);
        if from.x == to.x {
            cx.stroke(&Line::new(from, to), color, &stroke);
        } else {
            // An S curve between lanes, from two quadratics: vger renders cubics as dots.
            let middle = Point::new((from.x + to.x) / 2.0, (from.y + to.y) / 2.0);
            let mut path = BezPath::new();
            path.move_to(from);
            path.quad_to(Point::new(from.x, middle.y), middle);
            path.quad_to(Point::new(to.x, middle.y), to);
            cx.stroke(&path, color, &stroke);
        }
    }
    let circle = Circle::new(dot, 3.5);
    let color = lane_color(config, row.color);
    if row.commit.parents.len() > 1 {
        // Merges are hollow, like git-graph's.
        cx.fill(&circle, config.color(PlanckColor::PANEL_BACKGROUND), 0.0);
        cx.stroke(&circle, color, &stroke);
    } else {
        cx.fill(&circle, color, 0.0);
    }
}

fn row_view(
    row: Rc<Row>,
    config: ReadSignal<Arc<PlanckConfig>>,
    line_height: Memo<f64>,
    now: i64,
) -> impl View {
    let refs = if row.commit.refs.is_empty() {
        String::new()
    } else {
        format!("({})", row.commit.refs.join(", "))
    };
    let has_refs = !refs.is_empty();
    let summary = row.commit.info.summary.clone();
    let detail = format!(
        "{}, {}",
        row.commit.info.author,
        time_ago(now - row.commit.info.time)
    );
    let color = row.color;
    // Each row only as wide as its own lanes, like `git log --graph`.
    let width = row.width() as f64 * LANE_WIDTH;
    stack((
        canvas(move |cx, size| paint_row(cx, size, &row, &config.get())).style(
            move |s| s.width(width).height(line_height.get()).flex_shrink(0.0f32),
        ),
        label(move || refs.clone()).style(move |s| {
            s.color(lane_color(&config.get(), color))
                .font_weight(Weight::BOLD)
                .margin_left(4.0)
                .selectable(false)
                .apply_if(!has_refs, |s| s.hide())
        }),
        label(move || summary.clone())
            .style(|s| s.margin_left(4.0).selectable(false)),
        label(move || detail.clone()).style(move |s| {
            s.margin_left(10.0)
                .color(config.get().color(PlanckColor::EDITOR_DIM))
                .selectable(false)
        }),
    ))
    .style(move |s| {
        s.items_center()
            .height(line_height.get())
            .padding_left(6.0)
            .padding_right(10.0)
            .hover(|s| {
                s.background(
                    config.get().color(PlanckColor::PANEL_HOVERED_BACKGROUND),
                )
            })
    })
}

/// The workspace repository's history: every branch as a colored lane.
pub fn git_graph_panel(window_tab_data: Rc<WindowTabData>) -> impl View {
    let config = window_tab_data.common.config;
    let line_height = window_tab_data.common.ui_line_height;
    let scope = window_tab_data.common.scope;
    let proxy = window_tab_data.common.proxy.clone();
    let branch = window_tab_data.source_control.branch;
    let rows = scope.create_rw_signal(im::Vector::<Rc<Row>>::new());

    create_effect(move |_| {
        // Set on every DiffInfo, which the proxy sends whenever a ref moves.
        branch.track();
        let send = create_ext_action(scope, move |result| {
            if let Ok(ProxyResponse::GitGraphResponse { commits }) = result {
                rows.set(layout(commits).into_iter().map(Rc::new).collect());
            }
        });
        proxy.git_graph(send);
    });

    scroll(
        virtual_stack(
            move || rows.get(),
            // By value: a reload rebuilds only the rows that changed.
            |row| row.clone(),
            move |row| {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs() as i64);
                row_view(row, config, line_height, now)
            },
        )
        .item_size_fixed(move || line_height.get())
        .style(|s| s.flex_col().absolute().min_width_full()),
    )
    .style(|s| s.absolute().size_full())
}

#[cfg(test)]
mod tests {
    use planck_rpc::source_control::CommitInfo;

    use super::*;

    fn commit(id: &str, parents: &[&str]) -> GraphCommit {
        GraphCommit {
            info: CommitInfo {
                id: id.to_string(),
                author: String::new(),
                time: 0,
                summary: String::new(),
            },
            parents: parents.iter().map(|p| p.to_string()).collect(),
            refs: Vec::new(),
        }
    }

    #[test]
    fn layout_gives_a_merged_branch_its_own_lane() {
        // m merges feature f into main; both started at base b.
        let rows = layout(vec![
            commit("m", &["a", "f"]),
            commit("f", &["b"]),
            commit("a", &["b"]),
            commit("b", &[]),
        ]);
        let shape: Vec<_> = rows
            .iter()
            .map(|r| {
                let edges: Vec<_> = r.edges.iter().map(|(e, _)| *e).collect();
                (r.commit.info.id.as_str(), r.column, edges)
            })
            .collect();
        use Edge::*;
        assert_eq!(
            shape,
            [
                ("m", 0, vec![Out(0), Out(1)]),
                ("f", 1, vec![Pass(0), In(1), Out(1)]),
                ("a", 0, vec![In(0), Pass(1), Out(0)]),
                ("b", 0, vec![In(0), In(1)]),
            ]
        );
        // main keeps one color top to bottom, the feature lane gets another.
        assert_eq!(rows[0].color, rows[2].color);
        assert_ne!(rows[0].color, rows[1].color);
        assert_eq!(rows.iter().map(Row::width).max(), Some(2));
    }
}
