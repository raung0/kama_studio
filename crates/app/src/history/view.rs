use super::{HistorySnapshot, HistoryState};

use kama_ui::{BlockId, BuildCtx, Color, Rect, ScrollState, Size};

use crate::theme;

const ROW_H: f32 = 27.0;
const PAD: f32 = 6.0;
const GRAPH_LEFT: f32 = 12.0;
const LANE_W: f32 = 14.0;
const NODE_D: f32 = 8.0;
const LINE_W: f32 = 2.0;

#[derive(Default)]
pub struct HistoryPanelState {
    scroll: ScrollState,
}

impl HistoryPanelState {
    pub(crate) fn pointer_pressed(
        &mut self,
        history: &mut HistoryState,
        rect: Rect,
        point: [f32; 2],
    ) -> Option<HistorySnapshot> {
        if !rect.contains(point) {
            return None;
        }
        let row = history_row_rects(rect, history.len(), self.scroll)
            .iter()
            .position(|row| row.contains(point))?;
        let index = history.graph_rows().get(row)?.index;
        history.select(index)
    }

    pub(crate) fn scroll(
        &mut self,
        history: &HistoryState,
        rect: Rect,
        point: [f32; 2],
        delta: [f32; 2],
    ) -> bool {
        if !rect.contains(point) {
            return false;
        }
        let content = PAD.mul_add(2.0, history.len() as f32 * ROW_H);
        self.scroll
            .scroll_by(-delta[1], (content - rect.height).max(0.0))
    }

    pub(crate) fn build(&self, history: &HistoryState, ctx: &mut BuildCtx, rect: Rect) {
        let rect = Rect::new(0.0, 0.0, rect.width, rect.height);
        let rows = history.graph_rows();
        let row_rects = history_row_rects(rect, rows.len(), self.scroll);
        let max_lane = rows.iter().map(|row| row.lane).max().unwrap_or(0);
        let label_offset = (max_lane as f32 + 1.0).mul_add(LANE_W, GRAPH_LEFT) + 5.0;
        kama_ui::ui!(ctx, {
            Rect("history-panel-bg", rect) {
                fill: theme::panel();
            }
        });

        for (row_number, row_info) in rows.iter().enumerate() {
            let node = history
                .entry(row_info.index)
                .expect("layout row has an entry");
            let Some(parent) = node.parent else {
                continue;
            };
            let Some(parent_row) = rows.get(parent) else {
                continue;
            };
            let Some(parent_rect) = row_rects.get(parent) else {
                continue;
            };
            let Some(child_rect) = row_rects.get(row_number) else {
                continue;
            };
            let parent_y = f32::mul_add(parent_rect.height, 0.5, parent_rect.y);
            let child_y = f32::mul_add(child_rect.height, 0.5, child_rect.y);
            if child_y < -ROW_H || parent_y > rect.bottom() + ROW_H {
                continue;
            }

            let parent_x = history_lane_x(parent_row.lane);
            let child_x = history_lane_x(row_info.lane);
            let color = history_branch_color(row_info.branch);
            kama_ui::ui!(ctx, {
                @if (parent_x - child_x).abs() > f32::EPSILON {
                    Rect(("history-fork-horizontal", node.id), horizontal_line(parent_x, child_x, parent_y)) {
                        fill: color;
                    }
                }
                Rect(("history-edge", node.id), vertical_line(child_x, parent_y, child_y)) {
                    fill: color;
                }
            });
        }

        for (row_number, row_info) in rows.into_iter().enumerate() {
            let node = history
                .entry(row_info.index)
                .expect("layout row has an entry");
            let Some(row) = row_rects.get(row_number).copied() else {
                continue;
            };
            if row.bottom() < 0.0 || row.y > rect.bottom() {
                continue;
            }

            let selected = row_info.index == history.current();
            let node_x = history_lane_x(row_info.lane);
            let branch_color = history_branch_color(row_info.branch);
            kama_ui::ui!(ctx, {
                Rect(("history-row", node.id), row) {
                    row;
                    fill: if selected { theme::control().mix(theme::accent(), 0.18) } else { Color::TRANSPARENT };
                    border: u32::from(selected);
                    border_color: if selected { theme::accent().mix(theme::line_soft(), 0.45) } else { theme::line_soft() };
                    border_radius: 5.0; interactive;

                    Block {
                        width: Size::Pixels(label_offset);
                        height: Size::Fill;
                    }
                    Block {
                        id: @format("history-label-{}", node.id);
                        width: Size::Fill;
                        height: Size::Fill;
                        font_size: 10.5;
                        text_color: if selected { theme::text() } else { theme::muted() };
                        text: node.label;
                    }
                    Block { width: Size::Pixels(5.0); height: Size::Fill; }
                }
                Rect(("history-node", node.id), Rect::new(
                    NODE_D.mul_add(-0.5, node_x), NODE_D.mul_add(-0.5, f32::mul_add(row.height, 0.5, row.y)), NODE_D, NODE_D,
                )) {
                    fill: branch_color; border: if selected { 2 } else { 1 };
                    border_color: if selected { theme::text() } else { theme::panel() }; border_radius: NODE_D * 0.5;
                }
            });
        }
    }
}

impl HistoryState {
    fn graph_rows(&self) -> Vec<HistoryRow> {
        if self.graph.is_empty() {
            return Vec::new();
        }

        let mut rows = Vec::with_capacity(self.len());
        let mut lanes = vec![0usize; self.len()];
        let mut branches = vec![0usize; self.len()];
        let mut next_lane = 1usize;
        let mut next_branch = 1usize;
        rows.push(HistoryRow {
            index: 0,
            lane: 0,
            branch: 0,
        });

        for index in 1..self.len() {
            let parent = self
                .entry(index)
                .and_then(|entry| entry.parent)
                .unwrap_or(0);
            let continues_parent_branch =
                self.entry(parent).and_then(|entry| entry.first_child) == Some(index);
            let (lane, branch) = if continues_parent_branch {
                (lanes[parent], branches[parent])
            } else {
                let lane = next_lane;
                let branch = next_branch;
                next_lane += 1;
                next_branch += 1;
                (lane, branch)
            };
            lanes[index] = lane;
            branches[index] = branch;
            rows.push(HistoryRow {
                index,
                lane,
                branch,
            });
        }
        rows
    }
}

#[derive(Clone, Copy)]
struct HistoryRow {
    index: usize,
    lane: usize,
    branch: usize,
}

fn history_lane_x(lane: usize) -> f32 {
    LANE_W.mul_add(0.5, (lane as f32).mul_add(LANE_W, PAD + GRAPH_LEFT))
}

fn history_row_rects(viewport: Rect, count: usize, scroll: ScrollState) -> Vec<Rect> {
    let (ids, measured) = kama_ui::measure_layout(viewport, |ctx| {
        let mut ids = Vec::with_capacity(count);
        let _ = ctx
            .new()
            .width(Size::Fill)
            .height(Size::Fill)
            .padding(PAD)
            .gap(2.0)
            .vertical_scroll(scroll)
            .children(|ctx| {
                for _ in 0..count {
                    ids.push(
                        ctx.new()
                            .width(Size::Fill)
                            .height(Size::Pixels(ROW_H - 2.0))
                            .build(),
                    );
                }
            })
            .build();
        ids
    });
    ids.into_iter()
        .filter_map(|id: BlockId| measured.rect(id))
        .collect()
}

fn horizontal_line(a: f32, b: f32, y: f32) -> Rect {
    Rect::new(
        a.min(b),
        LINE_W.mul_add(-0.5, y),
        (a - b).abs().max(LINE_W),
        LINE_W,
    )
}

fn vertical_line(x: f32, a: f32, b: f32) -> Rect {
    Rect::new(
        LINE_W.mul_add(-0.5, x),
        a.min(b),
        LINE_W,
        (a - b).abs().max(LINE_W),
    )
}

fn history_branch_color(branch: usize) -> Color {
    let hue = (branch as f32).mul_add(0.618_034, 0.58).fract();
    let saturation = 0.66;
    let value = 0.92;
    let h = hue * 6.0;
    let sector = h.floor() as i32;
    let f = h - sector as f32;
    let p = value * (1.0 - saturation);
    let q = value * f32::mul_add(saturation, -f, 1.0);
    let t = value * f32::mul_add(saturation, -(1.0 - f), 1.0);
    let (r, g, b) = match sector.rem_euclid(6) {
        0 => (value, t, p),
        1 => (q, value, p),
        2 => (p, value, t),
        3 => (p, q, value),
        4 => (t, p, value),
        _ => (value, p, q),
    };
    Color::rgba(r, g, b, 1.0)
}
