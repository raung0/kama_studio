use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaStream {
    All,
    Video(usize),
    Audio(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct MediaSelection {
    media: MediaId,
    stream: MediaStream,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct CompositionSelection {
    composition: CompositionId,
    stream: MediaStream,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaDragItem {
    Media {
        media: MediaId,
        stream: MediaStream,
    },
    Composition {
        composition: CompositionId,
        stream: MediaStream,
    },
}

#[derive(Clone, Copy, Debug)]
struct MediaContextMenu {
    point: [f32; 2],
    target: MediaContextTarget,
}

#[derive(Default)]
pub struct MediaPanelState {
    selected: HashSet<MediaSelection>,
    primary: Option<MediaSelection>,
    selected_composition: Option<CompositionSelection>,
    expanded_compositions: HashSet<CompositionId>,
    expanded: HashSet<MediaId>,
    composition_expansion: HashMap<CompositionId, f32>,
    media_expansion: HashMap<MediaId, f32>,
    scroll_y: f32,
    context_menu: Option<MediaContextMenu>,
    cursor: [f32; 2],
}

#[derive(Clone, Debug)]
pub enum MediaAction {
    None,
    NewComposition,
    DuplicateComposition(CompositionId),
    RenameComposition(CompositionId),
    DeleteComposition(CompositionId),
    BeginDrag {
        items: Vec<MediaDragItem>,
        open_on_click: Option<CompositionId>,
    },
    Import,
    ImportClipboard,
    InsertSelected {
        items: Vec<MediaDragItem>,
    },
    ReplaceSelectedMedia {
        media: MediaId,
    },
    RemoveSelected {
        media: Vec<MediaId>,
    },
}

enum MediaListEntry<'a> {
    Composition {
        composition: &'a crate::project::Composition,
        stream: MediaStream,
        open_amount: f32,
    },
    Media {
        asset: &'a crate::project::MediaAsset,
        stream: MediaStream,
        open_amount: f32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MediaKeyboardSelection {
    Composition(CompositionSelection),
    Media(MediaSelection),
}

#[derive(Clone, Copy)]
enum MediaRowHit {
    Composition {
        selection: CompositionSelection,
        parent: bool,
        disclosure: bool,
    },
    Media(MediaSelection, bool),
}

#[allow(clippy::too_many_arguments)]
fn visit_media_rows<'a, B>(
    rect: Rect,
    project: &'a Project,
    expanded_compositions: &HashSet<CompositionId>,
    expanded_media: &HashSet<MediaId>,
    composition_expansion: &HashMap<CompositionId, f32>,
    media_expansion: &HashMap<MediaId, f32>,
    scroll_y: f32,
    mut visit: impl FnMut(MediaListEntry<'a>, Rect) -> std::ops::ControlFlow<B>,
) -> Option<B> {
    struct Pending<'a> {
        entry: MediaListEntry<'a>,
        row_height: f32,
        slot_height: f32,
        child_inset: f32,
    }

    let mut pending = Vec::new();
    for composition in &project.compositions {
        let expanded = expanded_compositions.contains(&composition.id);
        let open_amount = composition_expansion
            .get(&composition.id)
            .copied()
            .unwrap_or(expanded as u8 as f32);
        pending.push(Pending {
            entry: MediaListEntry::Composition {
                composition,
                stream: MediaStream::All,
                open_amount,
            },
            row_height: MEDIA_ROW_H,
            slot_height: MEDIA_ROW_H + MEDIA_ITEM_GAP,
            child_inset: 0.0,
        });
        if open_amount > 0.001 {
            for stream in [MediaStream::Video(0), MediaStream::Audio(0)] {
                let row_height = MEDIA_TRACK_H * open_amount;
                pending.push(Pending {
                    entry: MediaListEntry::Composition {
                        composition,
                        stream,
                        open_amount,
                    },
                    row_height,
                    slot_height: (MEDIA_TRACK_H + MEDIA_ITEM_GAP) * open_amount,
                    child_inset: 6.0,
                });
            }
        }
    }

    let mut media = project.media.iter().collect::<Vec<_>>();
    media.sort_by_cached_key(|asset| (media_kind_sort_key(asset.kind), asset.name.to_lowercase()));
    for asset in media {
        let expanded = expanded_media.contains(&asset.id);
        let open_amount = media_expansion
            .get(&asset.id)
            .copied()
            .unwrap_or(expanded as u8 as f32);
        pending.push(Pending {
            entry: MediaListEntry::Media {
                asset,
                stream: MediaStream::All,
                open_amount,
            },
            row_height: MEDIA_ROW_H,
            slot_height: MEDIA_ROW_H + MEDIA_ITEM_GAP,
            child_inset: 0.0,
        });
        if open_amount > 0.001 {
            for stream in media_streams(asset) {
                let row_height = MEDIA_TRACK_H * open_amount;
                pending.push(Pending {
                    entry: MediaListEntry::Media {
                        asset,
                        stream,
                        open_amount,
                    },
                    row_height,
                    slot_height: (MEDIA_TRACK_H + MEDIA_ITEM_GAP) * open_amount,
                    child_inset: 6.0,
                });
            }
        }
    }

    let viewport = kama_ui::layout::inset(rect, 4.0);
    let slots = kama_ui::layout::column(
        viewport,
        &pending
            .iter()
            .map(|item| kama_ui::layout::Item::height(item.slot_height))
            .collect::<Vec<_>>(),
        0.0,
        0.0,
        kama_ui::Align::Start,
        Some(kama_ui::ScrollState { offset: scroll_y }),
    );
    for (item, slot) in pending.into_iter().zip(slots) {
        let row = kama_ui::layout::column(
            slot,
            &[
                kama_ui::layout::Item::height(item.row_height),
                kama_ui::layout::Item::fill(),
            ],
            0.0,
            0.0,
            kama_ui::Align::Start,
            None,
        )[0];
        let row = if item.child_inset > 0.0 {
            kama_ui::layout::row(
                row,
                &[
                    kama_ui::layout::Item::width(item.child_inset),
                    kama_ui::layout::Item::fill(),
                ],
                0.0,
                0.0,
                kama_ui::Align::Start,
            )[1]
        } else {
            row
        };
        if let std::ops::ControlFlow::Break(value) = visit(item.entry, row) {
            return Some(value);
        }
    }
    None
}

fn media_kind_sort_key(kind: MediaKind) -> u8 {
    match kind {
        MediaKind::Image { .. } => 0,
        MediaKind::Video => 1,
        MediaKind::Audio => 2,
        MediaKind::Model3d => 3,
        MediaKind::WasmPlugin => 4,
        MediaKind::Unknown => 5,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MediaContextItem {
    NewComposition,
    DuplicateComposition,
    RenameComposition,
    DeleteComposition,
    Import,
    ImportClipboard,
    Insert,
    ReplaceSelectedMedia,
    Remove,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MediaContextTarget {
    Empty,
    Composition,
    Media,
    Stream,
}

const EMPTY_MEDIA_CONTEXT_ITEMS: [(MediaContextItem, &str); 3] = [
    (MediaContextItem::NewComposition, "New Composition…"),
    (MediaContextItem::Import, "Import media…"),
    (MediaContextItem::ImportClipboard, "Import from clipboard"),
];

const COMPOSITION_CONTEXT_ITEMS: [(MediaContextItem, &str); 4] = [
    (
        MediaContextItem::DuplicateComposition,
        "Duplicate Composition",
    ),
    (MediaContextItem::RenameComposition, "Rename Composition…"),
    (MediaContextItem::DeleteComposition, "Delete Composition"),
    (MediaContextItem::Insert, "Insert selected at playhead"),
];

const MEDIA_ASSET_CONTEXT_ITEMS: [(MediaContextItem, &str); 3] = [
    (MediaContextItem::Insert, "Insert selected at playhead"),
    (
        MediaContextItem::ReplaceSelectedMedia,
        "Replace selected media…",
    ),
    (MediaContextItem::Remove, "Remove selected media"),
];

const MEDIA_STREAM_CONTEXT_ITEMS: [(MediaContextItem, &str); 1] =
    [(MediaContextItem::Insert, "Insert selected at playhead")];

fn media_context_items(target: MediaContextTarget) -> &'static [(MediaContextItem, &'static str)] {
    match target {
        MediaContextTarget::Empty => &EMPTY_MEDIA_CONTEXT_ITEMS,
        MediaContextTarget::Composition => &COMPOSITION_CONTEXT_ITEMS,
        MediaContextTarget::Media => &MEDIA_ASSET_CONTEXT_ITEMS,
        MediaContextTarget::Stream => &MEDIA_STREAM_CONTEXT_ITEMS,
    }
}

fn media_context_icon(item: MediaContextItem) -> AppIcon {
    match item {
        MediaContextItem::NewComposition => AppIcon::Composition,
        MediaContextItem::DuplicateComposition => AppIcon::Copy,
        MediaContextItem::RenameComposition => AppIcon::Rename,
        MediaContextItem::DeleteComposition => AppIcon::Delete,
        MediaContextItem::Import => AppIcon::Import,
        MediaContextItem::ImportClipboard => AppIcon::Paste,
        MediaContextItem::Insert => AppIcon::Timeline,
        MediaContextItem::ReplaceSelectedMedia => AppIcon::Restore,
        MediaContextItem::Remove => AppIcon::Remove,
    }
}

impl MediaPanelState {
    pub fn selected(&self) -> Option<MediaId> {
        self.primary.map(|selection| selection.media)
    }

    pub fn selected_with_stream(&self) -> Option<(MediaId, MediaStream)> {
        self.primary
            .map(|selection| (selection.media, selection.stream))
    }

    pub fn selected_composition(&self) -> Option<CompositionId> {
        self.selected_composition
            .map(|selection| selection.composition)
    }

    pub fn select_composition(&mut self, composition: CompositionId) {
        self.selected.clear();
        self.primary = None;
        self.selected_composition = Some(CompositionSelection {
            composition,
            stream: MediaStream::All,
        });
        self.context_menu = None;
    }

    pub fn clear_selection(&mut self) {
        self.selected.clear();
        self.primary = None;
        self.selected_composition = None;
        self.context_menu = None;
    }

    pub fn handle_key(
        &mut self,
        rect: Rect,
        event: &KeyEvent,
        modifiers: ModifiersState,
        project: &Project,
    ) -> bool {
        if modifiers.control_key() || modifiers.super_key() || modifiers.alt_key() {
            return false;
        }
        let direction = match event.logical_key {
            Key::Named(NamedKey::ArrowUp) => -1isize,
            Key::Named(NamedKey::ArrowDown) => 1isize,
            _ => return false,
        };

        let mut rows = Vec::new();
        for composition in &project.compositions {
            rows.push(MediaKeyboardSelection::Composition(CompositionSelection {
                composition: composition.id,
                stream: MediaStream::All,
            }));
            if self.expanded_compositions.contains(&composition.id) {
                rows.extend(
                    [MediaStream::Video(0), MediaStream::Audio(0)].map(|stream| {
                        MediaKeyboardSelection::Composition(CompositionSelection {
                            composition: composition.id,
                            stream,
                        })
                    }),
                );
            }
        }
        let mut media = project.media.iter().collect::<Vec<_>>();
        media.sort_by_cached_key(|asset| {
            (media_kind_sort_key(asset.kind), asset.name.to_lowercase())
        });
        for asset in media {
            rows.push(MediaKeyboardSelection::Media(MediaSelection {
                media: asset.id,
                stream: MediaStream::All,
            }));
            if self.expanded.contains(&asset.id) {
                rows.extend(media_streams(asset).into_iter().map(|stream| {
                    MediaKeyboardSelection::Media(MediaSelection {
                        media: asset.id,
                        stream,
                    })
                }));
            }
        }
        if rows.is_empty() {
            return true;
        }

        let current = self
            .selected_composition
            .map(MediaKeyboardSelection::Composition)
            .or_else(|| self.primary.map(MediaKeyboardSelection::Media));
        let index = current
            .and_then(|selection| rows.iter().position(|row| *row == selection))
            .map(|index| (index as isize + direction).clamp(0, rows.len() as isize - 1) as usize)
            .unwrap_or(if direction < 0 { rows.len() - 1 } else { 0 });
        let selection = rows[index];
        self.selected.clear();
        self.primary = None;
        self.selected_composition = None;
        match selection {
            MediaKeyboardSelection::Composition(selection) => {
                self.selected_composition = Some(selection);
            }
            MediaKeyboardSelection::Media(selection) => {
                self.selected.insert(selection);
                self.primary = Some(selection);
            }
        }
        self.context_menu = None;
        self.ensure_keyboard_selection_visible(rect, selection, project);
        true
    }

    fn ensure_keyboard_selection_visible(
        &mut self,
        rect: Rect,
        selection: MediaKeyboardSelection,
        project: &Project,
    ) {
        let row = visit_media_rows(
            rect,
            project,
            &self.expanded_compositions,
            &self.expanded,
            &self.composition_expansion,
            &self.media_expansion,
            self.scroll_y,
            |entry, row| {
                let matches = match (entry, selection) {
                    (
                        MediaListEntry::Composition {
                            composition,
                            stream,
                            ..
                        },
                        MediaKeyboardSelection::Composition(target),
                    ) => composition.id == target.composition && stream == target.stream,
                    (
                        MediaListEntry::Media { asset, stream, .. },
                        MediaKeyboardSelection::Media(target),
                    ) => asset.id == target.media && stream == target.stream,
                    _ => false,
                };
                if matches {
                    std::ops::ControlFlow::Break(row)
                } else {
                    std::ops::ControlFlow::Continue(())
                }
            },
        );
        let Some(row) = row else {
            return;
        };
        let top = media_list_top(rect);
        let bottom = rect.bottom() - 4.0;
        if row.y < top {
            self.scroll_y = (self.scroll_y - (top - row.y)).max(0.0);
        } else if row.bottom() > bottom {
            self.scroll_y += row.bottom() - bottom;
        }
    }

    pub fn close_context_menu(&mut self) {
        self.context_menu = None;
    }

    fn selected_drag_items(&self, project: &Project) -> Vec<MediaDragItem> {
        if let Some(selection) = self.selected_composition {
            return vec![MediaDragItem::Composition {
                composition: selection.composition,
                stream: selection.stream,
            }];
        }
        project
            .media
            .iter()
            .flat_map(|asset| {
                std::iter::once(MediaStream::All)
                    .chain(media_streams(asset))
                    .filter_map(move |stream| {
                        self.selected
                            .contains(&MediaSelection {
                                media: asset.id,
                                stream,
                            })
                            .then_some(MediaDragItem::Media {
                                media: asset.id,
                                stream,
                            })
                    })
            })
            .collect()
    }

    fn selected_media_ids(&self, project: &Project) -> Vec<MediaId> {
        project
            .media
            .iter()
            .filter(|asset| {
                self.selected
                    .iter()
                    .any(|selection| selection.media == asset.id)
            })
            .map(|asset| asset.id)
            .collect()
    }

    pub fn tick(&mut self, dt: f32) {
        let step = 1.0 - (-30.0 * dt).exp();
        for (&id, amount) in &mut self.composition_expansion {
            let target = self.expanded_compositions.contains(&id) as u8 as f32;
            *amount += (target - *amount) * step;
            if (*amount - target).abs() < 0.001 {
                *amount = target;
            }
        }
        for (&id, amount) in &mut self.media_expansion {
            let target = self.expanded.contains(&id) as u8 as f32;
            *amount += (target - *amount) * step;
            if (*amount - target).abs() < 0.001 {
                *amount = target;
            }
        }
    }

    pub fn is_animating(&self) -> bool {
        self.composition_expansion.iter().any(|(&id, &amount)| {
            (amount - self.expanded_compositions.contains(&id) as u8 as f32).abs() > 0.001
        }) || self
            .media_expansion
            .iter()
            .any(|(&id, &amount)| (amount - self.expanded.contains(&id) as u8 as f32).abs() > 0.001)
    }

    pub fn build(&self, ctx: &mut kama_ui::BuildCtx, rect: Rect, project: &Project, icons: Icons) {
        let rect = Rect::new(0.0, 0.0, rect.width, rect.height);
        kama_ui::ui!(ctx, {
            Rect("media-bg", rect) {
                fill: theme::panel();
            }
        });
        visit_media_rows(
            rect,
            project,
            &self.expanded_compositions,
            &self.expanded,
            &self.composition_expansion,
            &self.media_expansion,
            self.scroll_y,
            |entry, row| {
                match entry {
                    MediaListEntry::Composition {
                        composition,
                        stream: MediaStream::All,
                        open_amount,
                    } => {
                        let selection = CompositionSelection {
                            composition: composition.id,
                            stream: MediaStream::All,
                        };
                        draw_composition_row(
                            ctx,
                            row,
                            composition,
                            self.selected_composition == Some(selection),
                            project.active_composition == composition.id,
                            open_amount,
                            icons,
                        );
                    }
                    MediaListEntry::Composition {
                        composition,
                        stream,
                        open_amount,
                    } => draw_media_stream_row(
                        ctx,
                        row,
                        "composition",
                        composition.id,
                        stream,
                        self.selected_composition
                            == Some(CompositionSelection {
                                composition: composition.id,
                                stream,
                            }),
                        false,
                        open_amount,
                        icons,
                    ),
                    MediaListEntry::Media {
                        asset,
                        stream: MediaStream::All,
                        open_amount,
                    } => draw_media_row(
                        ctx,
                        row,
                        asset,
                        self.selected.contains(&MediaSelection {
                            media: asset.id,
                            stream: MediaStream::All,
                        }),
                        open_amount,
                        icons,
                    ),
                    MediaListEntry::Media {
                        asset,
                        stream,
                        open_amount,
                    } => draw_media_stream_row(
                        ctx,
                        row,
                        "media",
                        asset.id,
                        stream,
                        self.selected.contains(&MediaSelection {
                            media: asset.id,
                            stream,
                        }),
                        true,
                        open_amount,
                        icons,
                    ),
                }
                std::ops::ControlFlow::<()>::Continue(())
            },
        );
        self.build_context_menu(ctx, rect, project, icons);
    }

    fn build_context_menu(
        &self,
        ctx: &mut kama_ui::BuildCtx,
        panel: Rect,
        project: &Project,
        icons: Icons,
    ) {
        let Some(menu) = self.context_menu else {
            return;
        };
        let context_items = media_context_items(menu.target);
        let rect = context_menu_rect(panel, menu.point, context_items.len());
        let has_media_selection = !self.selected_media_ids(project).is_empty();
        let has_composition_selection = self.selected_composition.is_some();
        let has_insert_selection = !self.selected_drag_items(project).is_empty();
        let items = context_items
            .iter()
            .map(|&(item, label)| ContextMenuItem {
                label,
                shortcut: None,
                icon: Some(media_context_icon(item)),
                enabled: match item {
                    MediaContextItem::NewComposition
                    | MediaContextItem::Import
                    | MediaContextItem::ImportClipboard => true,
                    MediaContextItem::DuplicateComposition
                    | MediaContextItem::RenameComposition => has_composition_selection,
                    MediaContextItem::DeleteComposition => {
                        has_composition_selection && project.compositions.len() > 1
                    }
                    MediaContextItem::Insert => has_insert_selection,
                    MediaContextItem::ReplaceSelectedMedia => self.primary.is_some(),
                    MediaContextItem::Remove => has_media_selection,
                },
            })
            .collect::<Vec<_>>();
        build_context_menu(ctx, "media", rect, self.cursor, &items, icons);
    }

    pub fn pointer_pressed(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        modifiers: ModifiersState,
        project: &Project,
    ) -> MediaAction {
        if !rect.contains(point) {
            self.context_menu = None;
            return MediaAction::None;
        }
        let local = [point[0] - rect.x, point[1] - rect.y];
        self.cursor = local;
        if let Some(menu) = self.context_menu {
            let context_items = media_context_items(menu.target);
            let menu_rect = context_menu_rect(
                Rect::new(0.0, 0.0, rect.width, rect.height),
                menu.point,
                context_items.len(),
            );
            if let Some(index) = context_menu_hit(menu_rect, local, context_items.len()) {
                self.context_menu = None;
                return match context_items.get(index).map(|item| item.0) {
                    Some(MediaContextItem::NewComposition) => MediaAction::NewComposition,
                    Some(MediaContextItem::DuplicateComposition) => self
                        .selected_composition
                        .map(|selection| MediaAction::DuplicateComposition(selection.composition))
                        .unwrap_or(MediaAction::None),
                    Some(MediaContextItem::RenameComposition) => self
                        .selected_composition
                        .map(|selection| MediaAction::RenameComposition(selection.composition))
                        .unwrap_or(MediaAction::None),
                    Some(MediaContextItem::DeleteComposition) => self
                        .selected_composition
                        .filter(|_| project.compositions.len() > 1)
                        .map(|selection| MediaAction::DeleteComposition(selection.composition))
                        .unwrap_or(MediaAction::None),
                    Some(MediaContextItem::Import) => MediaAction::Import,
                    Some(MediaContextItem::ImportClipboard) => MediaAction::ImportClipboard,
                    Some(MediaContextItem::Insert) => {
                        let items = self.selected_drag_items(project);
                        if items.is_empty() {
                            MediaAction::None
                        } else {
                            MediaAction::InsertSelected { items }
                        }
                    }
                    Some(MediaContextItem::ReplaceSelectedMedia) => self
                        .primary
                        .map(|selection| MediaAction::ReplaceSelectedMedia {
                            media: selection.media,
                        })
                        .unwrap_or(MediaAction::None),
                    Some(MediaContextItem::Remove) => {
                        let media = self.selected_media_ids(project);
                        if media.is_empty() {
                            MediaAction::None
                        } else {
                            MediaAction::RemoveSelected { media }
                        }
                    }
                    None => MediaAction::None,
                };
            }
            self.context_menu = None;
        }
        let hit = visit_media_rows(
            rect,
            project,
            &self.expanded_compositions,
            &self.expanded,
            &self.composition_expansion,
            &self.media_expansion,
            self.scroll_y,
            |entry, row| {
                if !row.contains(point) {
                    return std::ops::ControlFlow::Continue(());
                }
                let hit = match entry {
                    MediaListEntry::Composition {
                        composition,
                        stream,
                        ..
                    } => MediaRowHit::Composition {
                        selection: CompositionSelection {
                            composition: composition.id,
                            stream,
                        },
                        parent: stream == MediaStream::All,
                        disclosure: stream == MediaStream::All
                            && media_disclosure_rect(row).contains(point),
                    },
                    MediaListEntry::Media { asset, stream, .. } => MediaRowHit::Media(
                        MediaSelection {
                            media: asset.id,
                            stream,
                        },
                        stream == MediaStream::All
                            && media_disclosure_rect(row).contains(point)
                            && !media_streams(asset).is_empty(),
                    ),
                };
                std::ops::ControlFlow::Break(hit)
            },
        );
        match hit {
            Some(MediaRowHit::Composition {
                selection,
                parent,
                disclosure,
            }) => {
                if disclosure {
                    let was_expanded = self.expanded_compositions.contains(&selection.composition);
                    self.composition_expansion
                        .entry(selection.composition)
                        .or_insert(was_expanded as u8 as f32);
                    if !self.expanded_compositions.remove(&selection.composition) {
                        self.expanded_compositions.insert(selection.composition);
                    }
                    MediaAction::None
                } else {
                    self.select_composition_and_drag(selection, parent)
                }
            }
            Some(MediaRowHit::Media(selection, disclosure)) => {
                if disclosure {
                    let was_expanded = self.expanded.contains(&selection.media);
                    self.media_expansion
                        .entry(selection.media)
                        .or_insert(was_expanded as u8 as f32);
                    if !self.expanded.remove(&selection.media) {
                        self.expanded.insert(selection.media);
                    }
                    MediaAction::None
                } else {
                    self.select_and_drag(selection, modifiers, project)
                }
            }
            None => MediaAction::None,
        }
    }

    fn select_composition_and_drag(
        &mut self,
        selection: CompositionSelection,
        open_on_click: bool,
    ) -> MediaAction {
        self.selected.clear();
        self.primary = None;
        self.selected_composition = Some(selection);
        MediaAction::BeginDrag {
            items: vec![MediaDragItem::Composition {
                composition: selection.composition,
                stream: selection.stream,
            }],
            open_on_click: open_on_click.then_some(selection.composition),
        }
    }

    fn select_and_drag(
        &mut self,
        selection: MediaSelection,
        modifiers: ModifiersState,
        project: &Project,
    ) -> MediaAction {
        self.selected_composition = None;
        let additive = modifiers.control_key() || modifiers.super_key();
        if modifiers.shift_key() {
            let visible = project
                .media
                .iter()
                .flat_map(|asset| {
                    let mut rows = vec![MediaSelection {
                        media: asset.id,
                        stream: MediaStream::All,
                    }];
                    if self.expanded.contains(&asset.id) {
                        rows.extend(media_streams(asset).into_iter().map(|stream| {
                            MediaSelection {
                                media: asset.id,
                                stream,
                            }
                        }));
                    }
                    rows
                })
                .collect::<Vec<_>>();
            if let (Some(anchor), Some(target)) = (
                self.primary
                    .and_then(|primary| visible.iter().position(|row| *row == primary)),
                visible.iter().position(|row| *row == selection),
            ) {
                if !additive {
                    self.selected.clear();
                }
                let (start, end) = if anchor <= target {
                    (anchor, target)
                } else {
                    (target, anchor)
                };
                self.selected.extend(visible[start..=end].iter().copied());
            } else {
                self.selected.clear();
                self.selected.insert(selection);
            }
        } else if additive {
            if !self.selected.insert(selection) {
                self.selected.remove(&selection);
            }
        } else if !self.selected.contains(&selection) {
            self.selected.clear();
            self.selected.insert(selection);
        }
        self.primary = self
            .selected
            .contains(&selection)
            .then_some(selection)
            .or_else(|| self.selected.iter().copied().next());
        let items = self.selected_drag_items(project);
        if items.is_empty() {
            MediaAction::None
        } else {
            MediaAction::BeginDrag {
                items,
                open_on_click: None,
            }
        }
    }

    pub fn pointer_right_pressed(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        project: &Project,
    ) -> bool {
        if !rect.contains(point) {
            self.context_menu = None;
            return false;
        }
        let local = [point[0] - rect.x, point[1] - rect.y];
        self.cursor = local;
        let hit = visit_media_rows(
            rect,
            project,
            &self.expanded_compositions,
            &self.expanded,
            &self.composition_expansion,
            &self.media_expansion,
            self.scroll_y,
            |entry, row| {
                if !row.contains(point) {
                    return std::ops::ControlFlow::Continue(());
                }
                let hit = match entry {
                    MediaListEntry::Media { asset, stream, .. } => MediaRowHit::Media(
                        MediaSelection {
                            media: asset.id,
                            stream,
                        },
                        false,
                    ),
                    MediaListEntry::Composition {
                        composition,
                        stream,
                        ..
                    } => MediaRowHit::Composition {
                        selection: CompositionSelection {
                            composition: composition.id,
                            stream,
                        },
                        parent: stream == MediaStream::All,
                        disclosure: false,
                    },
                };
                std::ops::ControlFlow::Break(hit)
            },
        );
        let target = match hit {
            Some(MediaRowHit::Media(selection, _)) => {
                if !self.selected.contains(&selection) {
                    self.selected.clear();
                    self.selected.insert(selection);
                }
                self.primary = Some(selection);
                self.selected_composition = None;
                if selection.stream == MediaStream::All {
                    MediaContextTarget::Media
                } else {
                    MediaContextTarget::Stream
                }
            }
            Some(MediaRowHit::Composition { selection, .. }) => {
                self.selected.clear();
                self.primary = None;
                self.selected_composition = Some(selection);
                if selection.stream == MediaStream::All {
                    MediaContextTarget::Composition
                } else {
                    MediaContextTarget::Stream
                }
            }
            None => {
                self.selected.clear();
                self.primary = None;
                self.selected_composition = None;
                MediaContextTarget::Empty
            }
        };
        self.context_menu = Some(MediaContextMenu {
            point: local,
            target,
        });
        true
    }

    pub fn pointer_moved(&mut self, rect: Rect, point: [f32; 2]) -> bool {
        if self.context_menu.is_none() {
            return false;
        }
        self.cursor = [point[0] - rect.x, point[1] - rect.y];
        true
    }

    pub fn scroll(
        &mut self,
        rect: Rect,
        point: [f32; 2],
        delta: [f32; 2],
        project: &Project,
    ) -> bool {
        if !rect.contains(point) {
            return false;
        }
        let media_height: f32 = project
            .media
            .iter()
            .map(|asset| {
                let stream_count = if self.expanded.contains(&asset.id) {
                    media_streams(asset).len()
                } else {
                    0
                };
                MEDIA_ROW_H
                    + MEDIA_ITEM_GAP
                    + stream_count as f32 * (MEDIA_TRACK_H + MEDIA_ITEM_GAP)
            })
            .sum();
        let composition_height: f32 = project
            .compositions
            .iter()
            .map(|composition| {
                let stream_count = if self.expanded_compositions.contains(&composition.id) {
                    2
                } else {
                    0
                };
                MEDIA_ROW_H
                    + MEDIA_ITEM_GAP
                    + stream_count as f32 * (MEDIA_TRACK_H + MEDIA_ITEM_GAP)
            })
            .sum();
        let content_height = composition_height + media_height;
        let viewport = (rect.bottom() - media_list_top(rect)).max(1.0);
        let max_scroll = (content_height - viewport).max(0.0);
        self.scroll_y = (self.scroll_y - delta[1]).clamp(0.0, max_scroll);
        true
    }
}
