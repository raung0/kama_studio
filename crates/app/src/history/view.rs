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
pub(crate) struct HistoryPanelState {
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
        let content = history.len() as f32 * ROW_H + PAD * 2.0;
        self.scroll
            .scroll_by(-delta[1], (content - rect.height).max(0.0))
    }

    pub(crate) fn build(&self, history: &HistoryState, ctx: &mut BuildCtx, rect: Rect) {
        let rect = Rect::new(0.0, 0.0, rect.width, rect.height);
        let rows = history.graph_rows();
        let row_rects = history_row_rects(rect, rows.len(), self.scroll);
        let max_lane = rows.iter().map(|row| row.lane).max().unwrap_or(0);
        let label_offset = (max_lane as f32 + 1.0) * LANE_W + GRAPH_LEFT + 5.0;
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
            let parent_y = parent_rect.y + parent_rect.height * 0.5;
            let child_y = child_rect.y + child_rect.height * 0.5;
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
                    border: if selected { 1 } else { 0 };
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
                    node_x - NODE_D * 0.5, row.y + row.height * 0.5 - NODE_D * 0.5, NODE_D, NODE_D,
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
    PAD + GRAPH_LEFT + lane as f32 * LANE_W + LANE_W * 0.5
}

fn history_row_rects(viewport: Rect, count: usize, scroll: ScrollState) -> Vec<Rect> {
    let (ids, measured) = kama_ui::measure_layout(viewport, |ctx| {
        let mut ids = Vec::with_capacity(count);
        ctx.new()
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
        y - LINE_W * 0.5,
        (a - b).abs().max(LINE_W),
        LINE_W,
    )
}

fn vertical_line(x: f32, a: f32, b: f32) -> Rect {
    Rect::new(
        x - LINE_W * 0.5,
        a.min(b),
        LINE_W,
        (a - b).abs().max(LINE_W),
    )
}

fn history_branch_color(branch: usize) -> Color {
    let hue = (0.58 + branch as f32 * 0.618_034).fract();
    let saturation = 0.66;
    let value = 0.92;
    let h = hue * 6.0;
    let sector = h.floor() as i32;
    let f = h - sector as f32;
    let p = value * (1.0 - saturation);
    let q = value * (1.0 - saturation * f);
    let t = value * (1.0 - saturation * (1.0 - f));
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
