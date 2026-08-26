use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

const TAB_MOTION_DURATION: Duration = Duration::from_millis(220);
const SPLIT_COLLAPSE_DURATION: Duration = Duration::from_millis(220);
const MAXIMIZE_MOTION_DURATION: Duration = Duration::from_millis(180);

pub const TAB_BAR_HEIGHT: f32 = 26.0;
pub const TAB_SEPARATOR_HEIGHT: f32 = 1.0;
const TAB_BAR_LEFT_PADDING: f32 = 6.0;
const TAB_BAR_TOP_PADDING: f32 = 3.0;
const TAB_CONTENT_CHROME_WIDTH: f32 = 27.0;
pub const SPLITTER_SIZE: f32 = 5.0;
pub const MIN_PANEL_SIZE: f32 = 96.0;

pub use kama_ui_renderer::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StackId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SplitId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TabId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropZone {
    Center,
    Left,
    Right,
    Top,
    Bottom,
}

impl DropZone {
    const fn split(self) -> Option<(Axis, bool)> {
        match self {
            Self::Left => Some((Axis::Horizontal, true)),
            Self::Right => Some((Axis::Horizontal, false)),
            Self::Top => Some((Axis::Vertical, true)),
            Self::Bottom => Some((Axis::Vertical, false)),
            Self::Center => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Tab {
    pub id: TabId,
    pub title: String,
}

#[derive(Clone, Debug)]
pub struct DockTransfer {
    pub titles: Vec<String>,
    pub active: usize,
}

impl DockTransfer {
    #[must_use]
    pub fn into_layout_spec(self) -> DockLayoutSpec {
        DockLayoutSpec::StackActive {
            titles: self.titles,
            active: self.active,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Stack {
    pub id: StackId,
    pub tabs: Vec<Tab>,
    pub active: usize,
}

impl Stack {
    const fn empty(id: StackId) -> Self {
        Self {
            id,
            tabs: Vec::new(),
            active: 0,
        }
    }

    fn single(id: StackId, tab: Tab) -> Self {
        Self {
            id,
            tabs: vec![tab],
            active: 0,
        }
    }

    fn insert_tab(&mut self, tab: Tab, index: Option<usize>) {
        let index = index.unwrap_or(self.tabs.len()).min(self.tabs.len());
        self.tabs.insert(index, tab);
        self.active = index;
    }

    fn append(&mut self, source: Self) {
        let base = self.tabs.len();
        self.active = base + source.active.min(source.tabs.len().saturating_sub(1));
        self.tabs.extend(source.tabs);
    }

    #[must_use]
    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs
            .get(self.active.min(self.tabs.len().saturating_sub(1)))
    }
}

#[derive(Clone, Debug)]
pub enum DockNode {
    Split {
        id: SplitId,
        axis: Axis,
        ratio: f32,
        first: Box<Self>,
        second: Box<Self>,
    },
    Stack(Stack),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DockLayoutSpec {
    Stack(Vec<String>),
    StackActive {
        titles: Vec<String>,
        active: usize,
    },
    Split {
        axis: Axis,
        ratio: f32,
        first: Box<Self>,
        second: Box<Self>,
    },
}

impl DockLayoutSpec {
    pub fn stack(title: impl Into<String>) -> Self {
        Self::Stack(vec![title.into()])
    }

    #[must_use]
    pub fn split(axis: Axis, ratio: f32, first: Self, second: Self) -> Self {
        Self::Split {
            axis,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }
}

#[derive(Clone, Debug)]
pub struct StackLayout {
    pub stack: Stack,
    pub rect: Rect,
    pub tab_bar: Rect,

    pub tab_viewport: Rect,
    pub content: Rect,
    pub plus_rect: Rect,
    pub maximize_rect: Rect,
}

#[derive(Clone, Copy, Debug)]
pub struct TabHit {
    pub stack_id: StackId,
    pub tab_id: TabId,
    pub rect: Rect,
    pub index: usize,
    pub opacity: f32,
    pub closing: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct SplitterLayout {
    pub split_id: SplitId,
    pub axis: Axis,
    pub rect: Rect,
    pub parent_rect: Rect,
}

#[derive(Clone, Debug, Default)]
pub struct LayoutSnapshot {
    pub stacks: Vec<StackLayout>,
    pub tabs: Vec<TabHit>,
    pub splitters: Vec<SplitterLayout>,
}

impl LayoutSnapshot {
    #[must_use]
    pub fn stack_at(&self, point: [f32; 2]) -> Option<&StackLayout> {
        self.stacks
            .iter()
            .rev()
            .find(|stack| stack.rect.contains(point))
    }

    #[must_use]
    pub fn content_at(&self, point: [f32; 2]) -> Option<&StackLayout> {
        self.stacks
            .iter()
            .rev()
            .find(|stack| stack.content.contains(point))
    }

    #[must_use]
    pub fn tab_at(&self, point: [f32; 2]) -> Option<TabHit> {
        self.tabs.iter().copied().rev().find(|tab| {
            !tab.closing
                && tab.rect.contains(point)
                && self
                    .stack(tab.stack_id)
                    .is_some_and(|stack| stack.tab_viewport.contains(point))
        })
    }

    #[must_use]
    pub fn splitter_at(&self, point: [f32; 2]) -> Option<SplitterLayout> {
        self.splitters
            .iter()
            .copied()
            .rev()
            .find(|splitter| splitter.rect.contains(point))
    }

    #[must_use]
    pub fn plus_at(&self, point: [f32; 2]) -> Option<&StackLayout> {
        self.stacks
            .iter()
            .rev()
            .find(|stack| stack.tab_viewport.contains(point) && stack.plus_rect.contains(point))
    }

    #[must_use]
    pub fn maximize_at(&self, point: [f32; 2]) -> Option<&StackLayout> {
        self.stacks
            .iter()
            .rev()
            .find(|stack| stack.maximize_rect.width > 0.0 && stack.maximize_rect.contains(point))
    }

    #[must_use]
    pub fn stack(&self, id: StackId) -> Option<&StackLayout> {
        self.stacks.iter().find(|stack| stack.stack.id == id)
    }
}

#[derive(Clone, Copy)]
struct MaximizeMotion {
    stack: StackId,
    started: Instant,
    maximizing: bool,
}

pub struct DockState {
    pub root: DockNode,
    pub focused: Option<StackId>,
    maximized: Option<StackId>,
    maximize_motion: Option<MaximizeMotion>,
    next_id: u64,
    tab_widths: HashMap<TabId, f32>,
    tab_offsets: HashMap<TabId, f32>,
    plus_offsets: HashMap<StackId, f32>,
    tab_scroll: HashMap<StackId, f32>,
    layout_animating: bool,
    opening_tabs: HashMap<TabId, Instant>,
    closing_tabs: HashMap<TabId, (StackId, Instant)>,
    collapsing_splits: HashMap<SplitId, Instant>,
}

impl DockState {
    #[must_use]
    pub fn from_spec(spec: DockLayoutSpec) -> Self {
        let mut state = Self {
            root: DockNode::Stack(Stack::empty(StackId(0))),
            focused: None,
            maximized: None,
            maximize_motion: None,
            next_id: 1,
            tab_widths: HashMap::new(),
            tab_offsets: HashMap::new(),
            plus_offsets: HashMap::new(),
            tab_scroll: HashMap::new(),
            layout_animating: false,
            opening_tabs: HashMap::new(),
            closing_tabs: HashMap::new(),
            collapsing_splits: HashMap::new(),
        };
        state.root = state.build_spec(spec);
        state.focused = stack_id_of(&state.root);
        state
    }

    fn build_spec(&mut self, spec: DockLayoutSpec) -> DockNode {
        match spec {
            DockLayoutSpec::Stack(titles) => {
                self.build_spec(DockLayoutSpec::StackActive { titles, active: 0 })
            }
            DockLayoutSpec::StackActive { titles, active } => {
                let id = StackId(self.alloc_id());
                let tabs = titles
                    .into_iter()
                    .map(|title| self.tab(title))
                    .collect::<Vec<_>>();
                DockNode::Stack(Stack {
                    id,
                    active: active.min(tabs.len().saturating_sub(1)),
                    tabs,
                })
            }
            DockLayoutSpec::Split {
                axis,
                ratio,
                first,
                second,
            } => DockNode::Split {
                id: SplitId(self.alloc_id()),
                axis,
                ratio: ratio.clamp(0.05, 0.95),
                first: Box::new(self.build_spec(*first)),
                second: Box::new(self.build_spec(*second)),
            },
        }
    }

    #[must_use]
    pub fn layout_spec(&self) -> DockLayoutSpec {
        fn snapshot(node: &DockNode) -> DockLayoutSpec {
            match node {
                DockNode::Split {
                    axis,
                    ratio,
                    first,
                    second,
                    ..
                } => DockLayoutSpec::Split {
                    axis: *axis,
                    ratio: *ratio,
                    first: Box::new(snapshot(first)),
                    second: Box::new(snapshot(second)),
                },
                DockNode::Stack(stack) => DockLayoutSpec::StackActive {
                    titles: stack.tabs.iter().map(|tab| tab.title.clone()).collect(),
                    active: stack.active,
                },
            }
        }
        snapshot(&self.root)
    }

    pub fn single(title: impl Into<String>) -> Self {
        let mut state = Self {
            root: DockNode::Stack(Stack::empty(StackId(0))),
            focused: None,
            maximized: None,
            maximize_motion: None,
            next_id: 1,
            tab_widths: HashMap::new(),
            tab_offsets: HashMap::new(),
            plus_offsets: HashMap::new(),
            tab_scroll: HashMap::new(),
            layout_animating: false,
            opening_tabs: HashMap::new(),
            closing_tabs: HashMap::new(),
            collapsing_splits: HashMap::new(),
        };
        let id = StackId(state.alloc_id());
        let tab = state.tab(title.into());
        let stack = Stack::single(id, tab);
        state.focused = Some(stack.id);
        state.root = DockNode::Stack(stack);
        state
    }

    const fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    const fn tab(&mut self, title: String) -> Tab {
        Tab {
            id: TabId(self.alloc_id()),
            title,
        }
    }

    pub fn layout(&mut self, bounds: Rect) -> LayoutSnapshot {
        self.layout_with_tab_measure(bounds, |title| title.chars().count() as f32 * 5.8)
    }

    pub fn layout_with_tab_measure(
        &mut self,
        bounds: Rect,
        mut measure_tab: impl FnMut(&str) -> f32,
    ) -> LayoutSnapshot {
        let now = Instant::now();
        self.opening_tabs
            .retain(|_, started| now.duration_since(*started) < TAB_MOTION_DURATION);
        let finished: Vec<_> = self
            .closing_tabs
            .iter()
            .filter_map(|(&tab, &(stack, started))| {
                (now.duration_since(started) >= TAB_MOTION_DURATION).then_some((stack, tab))
            })
            .collect();
        for (stack, tab) in finished {
            self.finish_close_tab(stack, tab);
        }

        let maximized = self
            .maximized
            .and_then(|id| find_stack(&self.root, id).cloned());
        if self.maximized.is_some() && maximized.is_none() {
            self.maximized = None;
        }
        let animated = if let Some(motion) = self.maximize_motion {
            if find_stack(&self.root, motion.stack).is_none() {
                self.maximize_motion = None;
                None
            } else {
                let t = now.duration_since(motion.started).as_secs_f32()
                    / MAXIMIZE_MOTION_DURATION.as_secs_f32();
                if t >= 1.0 {
                    self.maximize_motion = None;
                    None
                } else {
                    let t = t.clamp(0.0, 1.0);
                    Some((motion, t * t * 2.0f32.mul_add(-t, 3.0)))
                }
            }
        } else {
            None
        };
        self.layout_animating = false;
        let show_maximize = node_counts(&self.root).1 > 1;
        let mut snapshot = LayoutSnapshot::default();
        let mut context = LayoutContext {
            snapshot: &mut snapshot,
            tab_widths: &mut self.tab_widths,
            tab_offsets: &mut self.tab_offsets,
            plus_offsets: &mut self.plus_offsets,
            tab_scroll: &mut self.tab_scroll,
            animating: &mut self.layout_animating,
            opening_tabs: &self.opening_tabs,
            closing_tabs: &self.closing_tabs,
            collapsing_splits: &mut self.collapsing_splits,
            show_maximize,
            measure_tab: &mut measure_tab,
            now,
        };
        if let Some((motion, t)) = animated {
            layout_node(&self.root, bounds, Some(motion.stack), &mut context);
            if let (Some(stack), Some(docked)) = (
                find_stack(&self.root, motion.stack),
                stack_rect(&self.root, bounds, motion.stack),
            ) {
                let rect = if motion.maximizing {
                    docked.lerp(bounds, t)
                } else {
                    bounds.lerp(docked, t)
                };
                layout_stack(stack, rect, &mut context);
            }
        } else if let Some(stack) = maximized.as_ref() {
            layout_stack(stack, bounds, &mut context);
        } else {
            layout_node(&self.root, bounds, None, &mut context);
        }
        if !self.collapsing_splits.is_empty()
            && self
                .collapsing_splits
                .values()
                .all(|started| now.duration_since(*started) >= SPLIT_COLLAPSE_DURATION)
        {
            self.normalize_root();
            self.collapsing_splits.clear();
        }
        snapshot
    }

    pub fn toggle_maximize(&mut self, stack_id: StackId) -> bool {
        let maximizing = if self.maximized == Some(stack_id) {
            false
        } else if find_stack(&self.root, stack_id).is_some() {
            true
        } else {
            return false;
        };
        self.maximized = maximizing.then_some(stack_id);
        self.maximize_motion = Some(MaximizeMotion {
            stack: stack_id,
            started: Instant::now(),
            maximizing,
        });
        if maximizing {
            self.focused = Some(stack_id);
        }
        true
    }

    #[must_use]
    pub const fn maximized_stack(&self) -> Option<StackId> {
        self.maximized
    }

    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.layout_animating
            || self.maximize_motion.is_some()
            || !self.opening_tabs.is_empty()
            || !self.closing_tabs.is_empty()
            || !self.collapsing_splits.is_empty()
    }

    pub fn activate_tab(&mut self, stack_id: StackId, tab_id: TabId) {
        let activated = if let Some(stack) = find_stack_mut(&mut self.root, stack_id) {
            if let Some(index) = stack.tabs.iter().position(|tab| tab.id == tab_id) {
                stack.active = index;
                true
            } else {
                false
            }
        } else {
            false
        };
        if activated {
            self.focused = Some(stack_id);
        }
    }

    pub fn add_tab(&mut self, stack_id: StackId, title: impl Into<String>) -> Option<TabId> {
        let tab = self.tab(title.into());
        let id = tab.id;
        find_stack_mut(&mut self.root, stack_id)?.insert_tab(tab, None);
        self.focused = Some(stack_id);
        self.opening_tabs.insert(id, Instant::now());
        Some(id)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        node_counts(&self.root).0 == 0
    }

    pub fn detach_tab(&mut self, stack_id: StackId, tab_id: TabId) -> Option<DockTransfer> {
        let belongs_to_stack = find_stack(&self.root, stack_id)
            .is_some_and(|stack| stack.tabs.iter().any(|tab| tab.id == tab_id));
        if !belongs_to_stack || self.closing_tabs.contains_key(&tab_id) {
            return None;
        }
        let tab = remove_tab(&mut self.root, tab_id)?;
        self.tab_widths.remove(&tab_id);
        self.tab_offsets.remove(&tab_id);
        self.opening_tabs.remove(&tab_id);
        self.closing_tabs.remove(&tab_id);
        self.normalize_root();
        self.focused = stack_id_of(&self.root);
        Some(DockTransfer {
            titles: vec![tab.title],
            active: 0,
        })
    }

    pub fn detach_stack(&mut self, stack_id: StackId) -> Option<DockTransfer> {
        let stack = take_stack(&mut self.root, stack_id)?;
        if stack.tabs.is_empty() {
            return None;
        }
        for tab in &stack.tabs {
            self.tab_widths.remove(&tab.id);
            self.tab_offsets.remove(&tab.id);
            self.opening_tabs.remove(&tab.id);
            self.closing_tabs.remove(&tab.id);
        }
        let transfer = DockTransfer {
            active: stack.active.min(stack.tabs.len().saturating_sub(1)),
            titles: stack.tabs.into_iter().map(|tab| tab.title).collect(),
        };
        self.normalize_root();
        self.focused = stack_id_of(&self.root);
        Some(transfer)
    }

    pub fn drop_external(
        &mut self,
        transfer: DockTransfer,
        target_stack: StackId,
        zone: DropZone,
        insert_index: Option<usize>,
    ) -> Option<StackId> {
        if transfer.titles.is_empty() {
            return None;
        }
        let active = transfer.active.min(transfer.titles.len().saturating_sub(1));
        let tabs = transfer
            .titles
            .into_iter()
            .map(|title| self.tab(title))
            .collect::<Vec<_>>();

        let focused = if zone == DropZone::Center {
            let target = self.resolve_stack(target_stack);
            let stack = find_stack_mut(&mut self.root, target)?;
            let index = insert_index
                .unwrap_or(stack.tabs.len())
                .min(stack.tabs.len());
            for (offset, tab) in tabs.into_iter().enumerate() {
                stack.tabs.insert(index + offset, tab);
            }
            stack.active = index + active;
            target
        } else {
            let id = StackId(self.alloc_id());
            let stack = Stack { id, tabs, active };
            self.place_stack(target_stack, stack, zone)
        };
        self.focused = Some(focused);
        Some(focused)
    }

    pub fn close_tab(&mut self, stack_id: StackId, tab_id: TabId) -> bool {
        let belongs_to_stack = find_stack(&self.root, stack_id)
            .is_some_and(|stack| stack.tabs.iter().any(|tab| tab.id == tab_id));
        let remaining_tabs = node_counts(&self.root)
            .0
            .saturating_sub(self.closing_tabs.len());
        if !belongs_to_stack || self.closing_tabs.contains_key(&tab_id) || remaining_tabs <= 1 {
            return false;
        }
        self.opening_tabs.remove(&tab_id);
        if find_stack(&self.root, stack_id).is_some_and(|stack| stack.tabs.len() == 1) {
            self.finish_close_tab(stack_id, tab_id)
        } else {
            self.closing_tabs.insert(tab_id, (stack_id, Instant::now()));
            true
        }
    }

    fn finish_close_tab(&mut self, stack_id: StackId, tab_id: TabId) -> bool {
        if remove_tab(&mut self.root, tab_id).is_none() {
            return false;
        }
        self.tab_widths.remove(&tab_id);
        self.tab_offsets.remove(&tab_id);
        self.closing_tabs.remove(&tab_id);
        self.track_collapses();

        self.focused = if find_stack(&self.root, stack_id).is_some() {
            Some(stack_id)
        } else {
            stack_id_of(&self.root)
        };
        true
    }

    pub fn focus_stack(&mut self, stack_id: StackId) -> bool {
        if find_stack(&self.root, stack_id).is_none() {
            return false;
        }
        self.focused = Some(stack_id);
        true
    }

    pub fn dock_tab_to_edge(
        &mut self,
        tab_id: TabId,
        source_stack: StackId,
        zone: DropZone,
    ) -> bool {
        if zone == DropZone::Center || node_counts(&self.root).0 <= 1 {
            return false;
        }
        let Some(source) = find_stack(&self.root, source_stack) else {
            return false;
        };
        if self.closing_tabs.contains_key(&tab_id)
            || !source.tabs.iter().any(|tab| tab.id == tab_id)
        {
            return false;
        }
        let Some(tab) = remove_tab(&mut self.root, tab_id) else {
            return false;
        };
        let id = StackId(self.alloc_id());
        self.wrap_edge(DockNode::Stack(Stack::single(id, tab)), zone);
        self.track_collapses();
        self.focused = Some(id);
        true
    }

    pub fn dock_stack_to_edge(&mut self, source_stack: StackId, zone: DropZone) -> bool {
        if zone == DropZone::Center || node_counts(&self.root).1 <= 1 {
            return false;
        }
        let Some(stack) = take_stack(&mut self.root, source_stack) else {
            return false;
        };
        let id = stack.id;
        self.wrap_edge(DockNode::Stack(stack), zone);
        self.track_collapses();
        self.focused = Some(id);
        true
    }

    fn wrap_edge(&mut self, node: DockNode, zone: DropZone) {
        let Some((axis, before)) = zone.split() else {
            return;
        };
        let root = std::mem::replace(&mut self.root, DockNode::Stack(Stack::empty(StackId(0))));
        let (first, second) = if before { (node, root) } else { (root, node) };
        self.root = DockNode::Split {
            id: SplitId(self.alloc_id()),
            axis,
            ratio: if before { 0.28 } else { 0.72 },
            first: Box::new(first),
            second: Box::new(second),
        };
    }

    pub fn set_split_ratio(&mut self, split_id: SplitId, cursor: [f32; 2], parent_rect: Rect) {
        let Some((axis, ratio)) = split_axis_ratio_mut(&mut self.root, split_id) else {
            return;
        };
        let (cursor, origin, size) = match axis {
            Axis::Horizontal => (cursor[0], parent_rect.x, parent_rect.width),
            Axis::Vertical => (cursor[1], parent_rect.y, parent_rect.height),
        };
        let usable = (size - SPLITTER_SIZE).max(1.0);
        let raw = SPLITTER_SIZE.mul_add(-0.5, cursor - origin) / usable;
        let min_ratio = (MIN_PANEL_SIZE / usable).min(0.45);
        *ratio = raw.clamp(min_ratio, 1.0 - min_ratio);
    }

    pub fn drop_tab(
        &mut self,
        tab_id: TabId,
        source_stack: StackId,
        target_stack: StackId,
        zone: DropZone,
        insert_index: Option<usize>,
    ) {
        let Some(source) = find_stack(&self.root, source_stack) else {
            return;
        };
        if self.closing_tabs.contains_key(&tab_id)
            || !source.tabs.iter().any(|tab| tab.id == tab_id)
            || source_stack == target_stack && zone != DropZone::Center && source.tabs.len() <= 1
        {
            return;
        }

        if source_stack == target_stack && zone == DropZone::Center {
            if let Some(stack) = find_stack_mut(&mut self.root, source_stack) {
                let Some(old_index) = stack.tabs.iter().position(|tab| tab.id == tab_id) else {
                    return;
                };
                let tab = stack.tabs.remove(old_index);
                stack.insert_tab(tab, insert_index);
                self.focused = Some(source_stack);
            }
            return;
        }

        let Some(tab) = remove_tab(&mut self.root, tab_id) else {
            return;
        };
        let focused = if zone == DropZone::Center {
            self.insert_tab(target_stack, tab, insert_index)
        } else {
            let id = StackId(self.alloc_id());
            self.place_stack(target_stack, Stack::single(id, tab), zone)
        };
        self.track_collapses();
        self.focused = Some(focused);
    }

    pub fn drop_stack(&mut self, source_stack: StackId, target_stack: StackId, zone: DropZone) {
        if source_stack == target_stack {
            return;
        }
        let Some(source) = take_stack(&mut self.root, source_stack) else {
            return;
        };
        let focused = self.place_stack(target_stack, source, zone);
        self.track_collapses();
        self.focused = Some(focused);
    }

    fn insert_tab(&mut self, target: StackId, tab: Tab, index: Option<usize>) -> StackId {
        let target = self.resolve_stack(target);
        find_stack_mut(&mut self.root, target)
            .expect("dock root must contain a stack")
            .insert_tab(tab, index);
        target
    }

    fn place_stack(&mut self, target: StackId, source: Stack, zone: DropZone) -> StackId {
        let Some((axis, before)) = zone.split() else {
            let target = self.resolve_stack(target);
            find_stack_mut(&mut self.root, target)
                .expect("dock root must contain a stack")
                .append(source);
            return target;
        };
        let source_id = source.id;
        let split_id = SplitId(self.alloc_id());
        match replace_stack_with_split(
            &mut self.root,
            target,
            axis,
            before,
            split_id,
            DockNode::Stack(source),
        ) {
            Ok(()) => source_id,
            Err(DockNode::Stack(source)) => {
                let target = self.resolve_stack(target);
                find_stack_mut(&mut self.root, target)
                    .expect("dock root must contain a stack")
                    .append(source);
                target
            }
            Err(DockNode::Split { .. }) => unreachable!(),
        }
    }

    fn resolve_stack(&self, preferred: StackId) -> StackId {
        find_stack(&self.root, preferred).map_or_else(
            || stack_id_of(&self.root).expect("dock root must contain a stack"),
            |stack| stack.id,
        )
    }

    fn track_collapses(&mut self) {
        mark_collapsing_splits(&self.root, &mut self.collapsing_splits, Instant::now());
    }

    pub fn scroll_tabs(&mut self, stack_id: StackId, delta: f32) -> bool {
        if find_stack(&self.root, stack_id).is_none() || delta.abs() < f32::EPSILON {
            return false;
        }
        let offset = self.tab_scroll.entry(stack_id).or_default();
        *offset = (*offset + delta).max(0.0);
        true
    }

    fn normalize_root(&mut self) {
        let old_root = std::mem::replace(&mut self.root, DockNode::Stack(Stack::empty(StackId(0))));
        self.root = normalize_node(old_root)
            .unwrap_or_else(|| DockNode::Stack(Stack::empty(StackId(self.alloc_id()))));
        let root = &self.root;
        self.plus_offsets
            .retain(|stack, _| find_stack(root, *stack).is_some());
        self.tab_scroll
            .retain(|stack, _| find_stack(root, *stack).is_some());
    }
}

struct LayoutContext<'a> {
    snapshot: &'a mut LayoutSnapshot,
    tab_widths: &'a mut HashMap<TabId, f32>,
    tab_offsets: &'a mut HashMap<TabId, f32>,
    plus_offsets: &'a mut HashMap<StackId, f32>,
    tab_scroll: &'a mut HashMap<StackId, f32>,
    animating: &'a mut bool,
    opening_tabs: &'a HashMap<TabId, Instant>,
    closing_tabs: &'a HashMap<TabId, (StackId, Instant)>,
    collapsing_splits: &'a mut HashMap<SplitId, Instant>,
    show_maximize: bool,
    measure_tab: &'a mut dyn FnMut(&str) -> f32,
    now: Instant,
}

fn layout_node(
    node: &DockNode,
    rect: Rect,
    excluded: Option<StackId>,
    context: &mut LayoutContext<'_>,
) {
    match node {
        DockNode::Split {
            id,
            axis,
            ratio,
            first,
            second,
        } => {
            let first_visible = node_counts(first).0 > 0;
            let second_visible = node_counts(second).0 > 0;
            if first_visible != second_visible {
                let started = *context.collapsing_splits.entry(*id).or_insert(context.now);
                let t = (context.now.duration_since(started).as_secs_f32()
                    / SPLIT_COLLAPSE_DURATION.as_secs_f32())
                .clamp(0.0, 1.0);
                let eased = t * t * 2.0f32.mul_add(-t, 3.0);
                *context.animating |= t < 1.0;
                let (first_rect, _, second_rect) =
                    split_rects(rect, *axis, ratio.clamp(0.05, 0.95));
                let (child, start) = if first_visible {
                    (first.as_ref(), first_rect)
                } else {
                    (second.as_ref(), second_rect)
                };
                layout_node(child, start.lerp(rect, eased), excluded, context);
            } else if first_visible {
                let (first_rect, splitter, second_rect) =
                    split_rects(rect, *axis, ratio.clamp(0.05, 0.95));
                layout_node(first, first_rect, excluded, context);
                if excluded.is_none() {
                    context.snapshot.splitters.push(SplitterLayout {
                        split_id: *id,
                        axis: *axis,
                        rect: splitter,
                        parent_rect: rect,
                    });
                }
                layout_node(second, second_rect, excluded, context);
            }
        }
        DockNode::Stack(stack) if excluded != Some(stack.id) => layout_stack(stack, rect, context),
        DockNode::Stack(_) => {}
    }
}

fn stack_rect(node: &DockNode, rect: Rect, stack_id: StackId) -> Option<Rect> {
    match node {
        DockNode::Stack(stack) => (stack.id == stack_id).then_some(rect),
        DockNode::Split {
            axis,
            ratio,
            first,
            second,
            ..
        } => {
            let (first_rect, _, second_rect) = split_rects(rect, *axis, ratio.clamp(0.05, 0.95));
            stack_rect(first, first_rect, stack_id)
                .or_else(|| stack_rect(second, second_rect, stack_id))
        }
    }
}

fn split_rects(rect: Rect, axis: Axis, ratio: f32) -> (Rect, Rect, Rect) {
    let mut first = rect;
    let mut splitter = rect;
    let mut second = rect;
    match axis {
        Axis::Horizontal => {
            first.width = (rect.width - SPLITTER_SIZE).max(0.0) * ratio;
            splitter.x = rect.x + first.width;
            splitter.width = SPLITTER_SIZE;
            second.x = splitter.right();
            second.width = (rect.right() - second.x).max(0.0);
        }
        Axis::Vertical => {
            first.height = (rect.height - SPLITTER_SIZE).max(0.0) * ratio;
            splitter.y = rect.y + first.height;
            splitter.height = SPLITTER_SIZE;
            second.y = splitter.bottom();
            second.height = (rect.bottom() - second.y).max(0.0);
        }
    }
    (first, splitter, second)
}

fn stack_regions(rect: Rect, show_maximize: bool) -> (Rect, Rect, Rect) {
    let ((tab_bar, content), measured) = crate::measure_layout(rect, |ctx| {
        let mut tab_bar = crate::BlockId(0);
        let mut content = crate::BlockId(0);
        let _ = ctx
            .new()
            .column()
            .width(crate::Size::Fill)
            .height(crate::Size::Fill)
            .children(|ctx| {
                tab_bar = ctx
                    .new()
                    .width(crate::Size::Fill)
                    .height(crate::Size::Pixels(TAB_BAR_HEIGHT))
                    .build();
                let _ = ctx
                    .new()
                    .width(crate::Size::Fill)
                    .height(crate::Size::Pixels(TAB_SEPARATOR_HEIGHT))
                    .build();
                content = ctx
                    .new()
                    .width(crate::Size::Fill)
                    .height(crate::Size::Fill)
                    .build();
            })
            .build();
        (tab_bar, content)
    });
    let tab_bar = measured.rect(tab_bar).expect("dock tab bar layout");
    let content = measured.rect(content).expect("dock content layout");
    let (viewport, measured) = crate::measure_layout(tab_bar, |ctx| {
        let mut viewport = crate::BlockId(0);
        let _ = ctx
            .new()
            .row()
            .width(crate::Size::Fill)
            .height(crate::Size::Fill)
            .children(|ctx| {
                viewport = ctx
                    .new()
                    .width(crate::Size::Fill)
                    .height(crate::Size::Fill)
                    .build();
                let _ = ctx
                    .new()
                    .width(crate::Size::Pixels(if show_maximize { 29.0 } else { 2.0 }))
                    .height(crate::Size::Fill)
                    .build();
            })
            .build();
        viewport
    });
    (
        tab_bar,
        content,
        measured.rect(viewport).expect("dock tab viewport layout"),
    )
}

fn tab_strip_layout(tab_bar: Rect, widths: &[f32], scroll: f32) -> (Vec<Rect>, Rect, f32) {
    let ((tabs, plus, trailing), measured) = crate::measure_layout(tab_bar, |ctx| {
        let mut tabs = Vec::with_capacity(widths.len());
        let mut plus = crate::BlockId(0);
        let mut trailing = crate::BlockId(0);
        let _ = ctx
            .new()
            .column()
            .width(crate::Size::Fill)
            .height(crate::Size::Fill)
            .children(|ctx| {
                let _ = ctx
                    .new()
                    .width(crate::Size::Fill)
                    .height(crate::Size::Pixels(TAB_BAR_TOP_PADDING))
                    .build();
                let _ = ctx
                    .new()
                    .row()
                    .width(crate::Size::Fill)
                    .height(crate::Size::Fill)
                    .horizontal_scroll(crate::ScrollState { offset: scroll })
                    .align_items(crate::Align::Start)
                    .children(|ctx| {
                        let _ = ctx
                            .new()
                            .width(crate::Size::Pixels(TAB_BAR_LEFT_PADDING))
                            .height(crate::Size::Fill)
                            .build();
                        for (index, width) in widths.iter().copied().enumerate() {
                            tabs.push(
                                ctx.new()
                                    .width(crate::Size::Pixels(width))
                                    .height(crate::Size::Fill)
                                    .build(),
                            );
                            if index + 1 < widths.len() {
                                let _ = ctx
                                    .new()
                                    .width(crate::Size::Pixels(1.0))
                                    .height(crate::Size::Fill)
                                    .build();
                            }
                        }
                        let _ = ctx
                            .new()
                            .width(crate::Size::Pixels(2.0))
                            .height(crate::Size::Fill)
                            .build();
                        plus = ctx
                            .new()
                            .width(crate::Size::Pixels(23.0))
                            .height(crate::Size::Pixels(
                                TAB_BAR_HEIGHT - TAB_BAR_TOP_PADDING - 2.0,
                            ))
                            .build();
                        trailing = ctx
                            .new()
                            .width(crate::Size::Pixels(2.0))
                            .height(crate::Size::Fill)
                            .build();
                    })
                    .build();
            })
            .build();
        (tabs, plus, trailing)
    });
    let tabs = tabs
        .into_iter()
        .map(|id| measured.rect(id).expect("dock tab layout"))
        .collect::<Vec<_>>();
    let plus = measured.rect(plus).expect("dock plus layout");
    let trailing = measured.rect(trailing).expect("dock strip trailing layout");
    let strip_width = trailing.right() + scroll - tab_bar.x;
    (tabs, plus, strip_width)
}

fn maximize_rect(tab_bar: Rect, visible: bool) -> Rect {
    let ((button, _), measured) = crate::measure_layout(tab_bar, |ctx| {
        let mut button = crate::BlockId(0);
        let mut row = crate::BlockId(0);
        let _ = ctx
            .new()
            .column()
            .width(crate::Size::Fill)
            .height(crate::Size::Fill)
            .children(|ctx| {
                let _ = ctx
                    .new()
                    .width(crate::Size::Fill)
                    .height(crate::Size::Pixels(TAB_BAR_TOP_PADDING))
                    .build();
                row = ctx
                    .new()
                    .row()
                    .width(crate::Size::Fill)
                    .height(crate::Size::Pixels(
                        TAB_BAR_HEIGHT - TAB_BAR_TOP_PADDING - 2.0,
                    ))
                    .children(|ctx| {
                        let _ = ctx
                            .new()
                            .width(crate::Size::Fill)
                            .height(crate::Size::Fill)
                            .build();
                        button = ctx
                            .new()
                            .width(crate::Size::Pixels(23.0))
                            .height(crate::Size::Fill)
                            .build();
                        let _ = ctx
                            .new()
                            .width(crate::Size::Pixels(4.0))
                            .height(crate::Size::Fill)
                            .build();
                    })
                    .build();
                let _ = ctx
                    .new()
                    .width(crate::Size::Fill)
                    .height(crate::Size::Pixels(2.0))
                    .build();
            })
            .build();
        (button, row)
    });
    let mut button = measured.rect(button).expect("dock maximize layout");
    if !visible {
        button.width = 0.0;
    }
    button
}

fn layout_stack(stack: &Stack, rect: Rect, context: &mut LayoutContext<'_>) {
    let (tab_bar, content_rect, tab_viewport) = stack_regions(rect, context.show_maximize);

    let mut widths = Vec::with_capacity(stack.tabs.len());
    for tab_item in &stack.tabs {
        let desired_width = (context.measure_tab)(&tab_item.title) + TAB_CONTENT_CHROME_WIDTH;
        let current_width = context
            .tab_widths
            .entry(tab_item.id)
            .or_insert(desired_width);
        *current_width = (desired_width - *current_width).mul_add(0.24, *current_width);
        *context.animating |= (*current_width - desired_width).abs() > 0.1;
        widths.push(*current_width);
    }
    let (_, _, strip_width) = tab_strip_layout(tab_bar, &widths, 0.0);
    let max_scroll = (strip_width - tab_viewport.width).max(0.0);
    let scroll = context.tab_scroll.entry(stack.id).or_default();
    *scroll = (*scroll).clamp(0.0, max_scroll);
    let (tab_targets, plus_target_rect, _) = tab_strip_layout(tab_bar, &widths, *scroll);

    for (index, ((tab, actual), target_rect)) in
        stack.tabs.iter().zip(widths).zip(tab_targets).enumerate()
    {
        let opening = context.opening_tabs.get(&tab.id).map_or(1.0, |started| {
            (context.now.duration_since(*started).as_secs_f32() / TAB_MOTION_DURATION.as_secs_f32())
                .clamp(0.0, 1.0)
        });
        let closing = context
            .closing_tabs
            .get(&tab.id)
            .map_or(0.0, |(_, started)| {
                (context.now.duration_since(*started).as_secs_f32()
                    / TAB_MOTION_DURATION.as_secs_f32())
                .clamp(0.0, 1.0)
            });
        let open_eased = 1.0 - (1.0 - opening).powi(3);
        let close_eased = closing.powi(3);

        let target_offset = target_rect.x - tab_bar.x;
        let animated_offset = context.tab_offsets.entry(tab.id).or_insert(target_offset);
        *animated_offset = (target_offset - *animated_offset).mul_add(0.22, *animated_offset);
        *context.animating |= (*animated_offset - target_offset).abs() > 0.1;
        let tab_rect = Rect::new(
            tab_bar.x + *animated_offset,
            (1.0 - open_eased).mul_add(12.0, target_rect.y) + close_eased * 16.0,
            actual,
            target_rect.height,
        );
        context.snapshot.tabs.push(TabHit {
            stack_id: stack.id,
            tab_id: tab.id,
            rect: tab_rect,
            index,
            opacity: open_eased * (1.0 - close_eased),
            closing: closing > 0.0,
        });
    }

    let plus_target = plus_target_rect.x - tab_bar.x;
    let plus_offset = context.plus_offsets.entry(stack.id).or_insert(plus_target);
    *plus_offset = (plus_target - *plus_offset).mul_add(0.22, *plus_offset);
    *context.animating |= (*plus_offset - plus_target).abs() > 0.1;
    let plus_rect = Rect {
        x: tab_bar.x + *plus_offset,
        ..plus_target_rect
    };
    context.snapshot.stacks.push(StackLayout {
        stack: stack.clone(),
        rect,
        tab_bar,
        tab_viewport,
        content: content_rect,
        plus_rect,
        maximize_rect: maximize_rect(tab_bar, context.show_maximize),
    });
}

fn stack_id_of(node: &DockNode) -> Option<StackId> {
    match node {
        DockNode::Stack(stack) => (!stack.tabs.is_empty()).then_some(stack.id),
        DockNode::Split { first, second, .. } => stack_id_of(first).or_else(|| stack_id_of(second)),
    }
}

fn node_counts(node: &DockNode) -> (usize, usize) {
    match node {
        DockNode::Stack(stack) => (stack.tabs.len(), usize::from(!stack.tabs.is_empty())),
        DockNode::Split { first, second, .. } => {
            let first = node_counts(first);
            let second = node_counts(second);
            (first.0 + second.0, first.1 + second.1)
        }
    }
}

fn find_node_mut<'a>(
    node: &'a mut DockNode,
    predicate: &impl Fn(&DockNode) -> bool,
) -> Option<&'a mut DockNode> {
    if predicate(node) {
        return Some(node);
    }
    match node {
        DockNode::Stack(_) => None,
        DockNode::Split { first, second, .. } => {
            find_node_mut(first, predicate).or_else(|| find_node_mut(second, predicate))
        }
    }
}

fn find_stack(node: &DockNode, id: StackId) -> Option<&Stack> {
    match node {
        DockNode::Stack(stack) => (stack.id == id).then_some(stack),
        DockNode::Split { first, second, .. } => {
            find_stack(first, id).or_else(|| find_stack(second, id))
        }
    }
}

fn find_stack_mut(node: &mut DockNode, id: StackId) -> Option<&mut Stack> {
    match find_node_mut(
        node,
        &|node| matches!(node, DockNode::Stack(stack) if stack.id == id),
    )? {
        DockNode::Stack(stack) => Some(stack),
        _ => unreachable!(),
    }
}

fn split_axis_ratio_mut(node: &mut DockNode, id: SplitId) -> Option<(Axis, &mut f32)> {
    match find_node_mut(
        node,
        &|node| matches!(node, DockNode::Split { id: split, .. } if *split == id),
    )? {
        DockNode::Split { axis, ratio, .. } => Some((*axis, ratio)),
        _ => unreachable!(),
    }
}

fn take_stack(node: &mut DockNode, id: StackId) -> Option<Stack> {
    let DockNode::Stack(stack) = find_node_mut(
        node,
        &|node| matches!(node, DockNode::Stack(stack) if stack.id == id),
    )?
    else {
        unreachable!()
    };
    Some(std::mem::replace(stack, Stack::empty(StackId(0))))
}

fn remove_tab(node: &mut DockNode, tab_id: TabId) -> Option<Tab> {
    let DockNode::Stack(stack) = find_node_mut(
        node,
        &|node| matches!(node, DockNode::Stack(stack) if stack.tabs.iter().any(|tab| tab.id == tab_id)),
    )?
    else {
        unreachable!()
    };
    let index = stack.tabs.iter().position(|tab| tab.id == tab_id)?;
    let tab = stack.tabs.remove(index);
    if stack.tabs.is_empty() {
        stack.id = StackId(0);
        stack.active = 0;
    } else if stack.active >= stack.tabs.len() {
        stack.active = stack.tabs.len() - 1;
    } else if index < stack.active {
        stack.active -= 1;
    }
    Some(tab)
}

fn mark_collapsing_splits(
    node: &DockNode,
    collapsing: &mut HashMap<SplitId, Instant>,
    now: Instant,
) -> bool {
    match node {
        DockNode::Stack(stack) => !stack.tabs.is_empty(),
        DockNode::Split {
            id, first, second, ..
        } => {
            let first_visible = mark_collapsing_splits(first, collapsing, now);
            let second_visible = mark_collapsing_splits(second, collapsing, now);
            if first_visible != second_visible {
                collapsing.entry(*id).or_insert(now);
            }
            first_visible || second_visible
        }
    }
}

fn normalize_node(node: DockNode) -> Option<DockNode> {
    match node {
        DockNode::Stack(stack) => (!stack.tabs.is_empty()).then_some(DockNode::Stack(stack)),
        DockNode::Split {
            id,
            axis,
            ratio,
            first,
            second,
        } => {
            let first = normalize_node(*first);
            let second = normalize_node(*second);
            match (first, second) {
                (Some(first), Some(second)) => Some(DockNode::Split {
                    id,
                    axis,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(node), None) | (None, Some(node)) => Some(node),
                (None, None) => None,
            }
        }
    }
}

fn replace_stack_with_split(
    node: &mut DockNode,
    target: StackId,
    axis: Axis,
    before: bool,
    split_id: SplitId,
    new_stack: DockNode,
) -> Result<(), DockNode> {
    let Some(node) = find_node_mut(
        node,
        &|node| matches!(node, DockNode::Stack(stack) if stack.id == target),
    ) else {
        return Err(new_stack);
    };
    let old = std::mem::replace(node, DockNode::Stack(Stack::empty(StackId(0))));
    let (first, second) = if before {
        (new_stack, old)
    } else {
        (old, new_stack)
    };
    *node = DockNode::Split {
        id: split_id,
        axis,
        ratio: 0.5,
        first: Box::new(first),
        second: Box::new(second),
    };
    Ok(())
}

#[must_use]
pub fn drop_zone(rect: Rect, point: [f32; 2]) -> DropZone {
    let local_x = (point[0] - rect.x) / rect.width.max(1.0);
    let local_y = (point[1] - rect.y) / rect.height.max(1.0);
    let edge = 0.24;
    if local_x < edge {
        DropZone::Left
    } else if local_x > 1.0 - edge {
        DropZone::Right
    } else if local_y < edge {
        DropZone::Top
    } else if local_y > 1.0 - edge {
        DropZone::Bottom
    } else {
        DropZone::Center
    }
}

#[must_use]
pub fn drop_preview(rect: Rect, zone: DropZone) -> Rect {
    if zone == DropZone::Center {
        return rect.inset(6.0);
    }
    let mut preview = rect;
    match zone {
        DropZone::Left => preview.width *= 0.5,
        DropZone::Right => {
            preview.x = preview.width.mul_add(0.5, preview.x);
            preview.width *= 0.5;
        }
        DropZone::Top => preview.height *= 0.5,
        DropZone::Bottom => {
            preview.y = preview.height.mul_add(0.5, preview.y);
            preview.height *= 0.5;
        }
        DropZone::Center => unreachable!(),
    }
    preview.inset(4.0)
}

#[must_use]
pub fn insertion_index(
    snapshot: &LayoutSnapshot,
    stack_id: StackId,
    dragged_tab: TabId,
    cursor_x: f32,
) -> usize {
    let mut tabs: Vec<_> = snapshot
        .tabs
        .iter()
        .filter(|tab| tab.stack_id == stack_id && tab.tab_id != dragged_tab)
        .collect();
    tabs.sort_by(|a, b| a.rect.x.total_cmp(&b.rect.x));
    for (index, tab) in tabs.iter().enumerate() {
        if cursor_x < tab.rect.width.mul_add(0.5, tab.rect.x) {
            return index;
        }
    }
    tabs.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_handles_follow_measured_title_width_without_a_cap() {
        let mut dock =
            DockState::from_spec(DockLayoutSpec::Stack(vec!["Short".into(), "Long".into()]));
        let snapshot =
            dock.layout_with_tab_measure(Rect::new(0.0, 0.0, 500.0, 300.0), |title| match title {
                "Short" => 42.0,
                "Long" => 180.0,
                _ => unreachable!(),
            });

        assert_eq!(snapshot.tabs[0].rect.width, 42.0 + TAB_CONTENT_CHROME_WIDTH);
        assert_eq!(
            snapshot.tabs[1].rect.width,
            180.0 + TAB_CONTENT_CHROME_WIDTH
        );
        assert!(snapshot.tabs[1].rect.width > 148.0);
    }

    #[test]
    fn content_hit_testing_never_claims_tab_bar() {
        let mut dock =
            DockState::from_spec(DockLayoutSpec::Stack(vec!["One".into(), "Two".into()]));
        let snapshot = dock.layout(Rect::new(0.0, 0.0, 500.0, 300.0));
        let tab = snapshot.tabs[0];
        let point = [
            tab.rect.width.mul_add(0.5, tab.rect.x),
            tab.rect.height.mul_add(0.5, tab.rect.y),
        ];
        assert!(snapshot.stack_at(point).is_some());
        assert!(snapshot.content_at(point).is_none());
        assert_eq!(
            snapshot.tab_at(point).map(|hit| hit.tab_id),
            Some(tab.tab_id)
        );
    }

    #[test]
    fn transfer_can_empty_one_dock_and_join_another() {
        let mut source = DockState::single("Inspector");
        let source_snapshot = source.layout(Rect::new(0.0, 0.0, 400.0, 300.0));
        let source_tab = source_snapshot.tabs[0];
        let transfer = source
            .detach_tab(source_tab.stack_id, source_tab.tab_id)
            .expect("detach source tab");
        assert!(source.is_empty());
        assert_eq!(transfer.titles, vec!["Inspector".to_string()]);

        let mut target = DockState::single("Timeline");
        let target_snapshot = target.layout(Rect::new(0.0, 0.0, 400.0, 300.0));
        let target_stack = target_snapshot.stacks[0].stack.id;
        assert!(
            target
                .drop_external(transfer, target_stack, DropZone::Center, None)
                .is_some()
        );

        let result = target.layout(Rect::new(0.0, 0.0, 400.0, 300.0));
        let stack = &result.stacks[0].stack;
        assert_eq!(
            stack
                .tabs
                .iter()
                .map(|tab| tab.title.as_str())
                .collect::<Vec<_>>(),
            vec!["Timeline", "Inspector"]
        );
        assert_eq!(
            stack.active_tab().map(|tab| tab.title.as_str()),
            Some("Inspector")
        );
    }

    #[test]
    fn whole_stack_transfer_preserves_active_tab() {
        let mut source = DockState::from_spec(DockLayoutSpec::StackActive {
            titles: vec!["Media".into(), "Monitor".into()],
            active: 1,
        });
        let source_snapshot = source.layout(Rect::new(0.0, 0.0, 400.0, 300.0));
        let source_stack = source_snapshot.stacks[0].stack.id;
        let transfer = source
            .detach_stack(source_stack)
            .expect("detach source pane");
        assert!(source.is_empty());
        assert_eq!(transfer.active, 1);

        let mut target = DockState::single("Timeline");
        let target_snapshot = target.layout(Rect::new(0.0, 0.0, 500.0, 300.0));
        let target_stack = target_snapshot.stacks[0].stack.id;
        let inserted = target
            .drop_external(transfer, target_stack, DropZone::Right, None)
            .expect("drop source pane");
        let result = target.layout(Rect::new(0.0, 0.0, 500.0, 300.0));
        let inserted_stack = result.stack(inserted).expect("inserted stack");
        assert_eq!(
            inserted_stack
                .stack
                .active_tab()
                .map(|tab| tab.title.as_str()),
            Some("Monitor")
        );
    }
}
