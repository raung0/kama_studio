use super::*;

pub(super) fn graph_image_socket_label(kind: PipelineKind) -> &'static str {
    if kind == PipelineKind::Audio {
        "Audio"
    } else {
        "Image"
    }
}

pub(super) fn graph_property_label_parts(
    label: Rect,
    unique: bool,
    scale: f32,
) -> (Rect, Option<Rect>) {
    if !unique {
        return (label, None);
    }
    let parts = crate::ui_layout::row(
        label,
        &[
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::width(8.0 * scale),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    );
    (parts[0], Some(parts[1]))
}

pub(super) fn graph_card_header_parts(card: Rect) -> (Rect, Rect) {
    let scale = graph_card_scale(card);
    let header = crate::ui_layout::column(
        card,
        &[
            crate::ui_layout::Item::height(23.0 * scale),
            crate::ui_layout::Item::fill(),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
        None,
    )[0];
    let title = crate::ui_layout::row(
        header,
        &[
            crate::ui_layout::Item::width(7.0 * scale),
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::width(7.0 * scale),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    )[1];
    (header, title)
}

pub(super) fn graph_image_input_row(card: Rect, index: usize) -> Rect {
    let scale = graph_card_scale(card);
    let mut items = Vec::with_capacity(index + 2);
    items.push(crate::ui_layout::Item::height(27.0 * scale));
    items.extend(std::iter::repeat_n(
        crate::ui_layout::Item::height(GRAPH_IMAGE_INPUT_H * scale),
        index + 1,
    ));
    crate::ui_layout::column(card, &items, 0.0, 0.0, kama_ui::Align::Start, None)[index + 1]
}

pub(super) fn graph_value_input_row(card: Rect, index: usize) -> Rect {
    let scale = graph_card_scale(card);
    let mut items = Vec::with_capacity(index + 2);
    items.push(crate::ui_layout::Item::height(GRAPH_CARD_BASE_H * scale));
    items.extend(std::iter::repeat_n(
        crate::ui_layout::Item::height(GRAPH_INPUT_H * scale),
        index + 1,
    ));
    crate::ui_layout::column(card, &items, 0.0, 0.0, kama_ui::Align::Start, None)[index + 1]
}

pub(super) fn graph_card_content_rows(card_rect: Rect, card: &GraphCard) -> (Vec<Rect>, Vec<Rect>) {
    let scale = graph_card_scale(card_rect);
    let base =
        GRAPH_CARD_BASE_H + card.image_inputs.len().saturating_sub(1) as f32 * GRAPH_IMAGE_INPUT_H;
    let mut items = Vec::with_capacity(1 + card.inputs.len() + card.host_inputs.len());
    items.push(crate::ui_layout::Item::height(base * scale));
    items.extend(card.inputs.iter().map(|input| {
        crate::ui_layout::Item::height(graph_property_row_height(input.definition.as_ref()) * scale)
    }));
    items.extend(
        card.host_inputs
            .iter()
            .map(|input| crate::ui_layout::Item::height(graph_host_row_height(input) * scale)),
    );
    let rows = crate::ui_layout::column(card_rect, &items, 0.0, 0.0, kama_ui::Align::Start, None);
    let property_end = 1 + card.inputs.len();
    (
        rows[1..property_end].to_vec(),
        rows[property_end..].to_vec(),
    )
}

pub(super) fn graph_image_input_label_rect(card: Rect, index: usize) -> Rect {
    let scale = graph_card_scale(card);
    let row = graph_image_input_row(card, index);
    crate::ui_layout::row(
        row,
        &[
            crate::ui_layout::Item::width(9.0 * scale),
            crate::ui_layout::Item::new(Size::Fill, Size::Pixels(14.0 * scale)),
            crate::ui_layout::Item::width(9.0 * scale),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    )[1]
}

pub(super) fn graph_output_label_rect(card: Rect) -> Rect {
    let scale = graph_card_scale(card);
    let row = graph_image_input_row(card, 0);
    crate::ui_layout::row(
        row,
        &[
            crate::ui_layout::Item::new(Size::Fill, Size::Pixels(14.0 * scale)),
            crate::ui_layout::Item::new(Size::Pixels(48.0 * scale), Size::Pixels(14.0 * scale)),
            crate::ui_layout::Item::new(Size::Pixels(9.0 * scale), Size::Pixels(14.0 * scale)),
        ],
        0.0,
        0.0,
        kama_ui::Align::Center,
    )[1]
}

pub(super) fn graph_value_detail_rect(card: Rect) -> Rect {
    let rows = crate::ui_layout::stack(card, card.y, &[29.0, 16.0]);
    crate::ui_layout::row(
        rows[1],
        &[
            crate::ui_layout::Item::width(9.0),
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::width(9.0),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    )[1]
}

pub(super) fn graph_image_input_port(card: Rect) -> Rect {
    graph_named_image_input_port(card, 0)
}

pub(super) fn graph_named_image_input_port(card: Rect, index: usize) -> Rect {
    let scale = graph_card_scale(card);
    Rect::new(
        card.x - 5.0 * scale,
        card.y + (29.0 + index as f32 * GRAPH_IMAGE_INPUT_H) * scale,
        10.0 * scale,
        10.0 * scale,
    )
}

pub(super) fn graph_image_output_port(card: Rect) -> Rect {
    let scale = graph_card_scale(card);
    Rect::new(
        card.right() - 5.0 * scale,
        card.y + 29.0 * scale,
        10.0 * scale,
        10.0 * scale,
    )
}

pub(super) fn graph_image_input_point(card: Rect) -> [f32; 2] {
    graph_named_image_input_point(card, 0)
}

pub(super) fn graph_named_image_input_point(card: Rect, index: usize) -> [f32; 2] {
    let port = graph_named_image_input_port(card, index);
    [port.x + port.width * 0.5, port.y + port.height * 0.5]
}

pub(super) fn graph_image_output_point(card: Rect) -> [f32; 2] {
    let port = graph_image_output_port(card);
    [port.x + port.width * 0.5, port.y + port.height * 0.5]
}

pub(super) fn graph_value_output_port(card: Rect) -> Rect {
    graph_image_output_port(card)
}

pub(super) fn graph_value_output_point(card: Rect) -> [f32; 2] {
    let port = graph_value_output_port(card);
    [port.x + port.width * 0.5, port.y + port.height * 0.5]
}

pub(super) fn graph_property_row_rect(
    card_rect: Rect,
    card: &GraphCard,
    input_index: usize,
) -> Rect {
    graph_card_content_rows(card_rect, card).0[input_index]
}

pub(super) fn graph_host_row_rect(card_rect: Rect, card: &GraphCard, host_index: usize) -> Rect {
    graph_card_content_rows(card_rect, card).1[host_index]
}

pub(super) fn graph_host_eq_parts(row: Rect, scale: f32) -> (Rect, Rect, Rect) {
    let top = crate::ui_layout::row(
        row,
        &[
            crate::ui_layout::Item::width(8.0 * scale),
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::width(4.0 * scale),
            crate::ui_layout::Item::width(16.0 * scale),
            crate::ui_layout::Item::width(6.0 * scale),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    );
    let label = crate::ui_layout::column(
        top[1],
        &[
            crate::ui_layout::Item::height(2.0 * scale),
            crate::ui_layout::Item::height(15.0 * scale),
            crate::ui_layout::Item::fill(),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
        None,
    )[1];
    let keyframe = crate::ui_layout::column(
        top[3],
        &[
            crate::ui_layout::Item::height(1.0 * scale),
            crate::ui_layout::Item::height(16.0 * scale),
            crate::ui_layout::Item::fill(),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
        None,
    )[1];
    let vertical = crate::ui_layout::column(
        row,
        &[
            crate::ui_layout::Item::height(19.0 * scale),
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::height(4.0 * scale),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
        None,
    );
    let viewport = crate::ui_layout::row(
        vertical[1],
        &[
            crate::ui_layout::Item::width(7.0 * scale),
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::width(7.0 * scale),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    )[1];
    (label, keyframe, viewport)
}

pub(super) fn graph_host_value_parts(row: Rect, scale: f32) -> (Rect, Rect) {
    let parts = crate::ui_layout::row(
        row,
        &[
            crate::ui_layout::Item::width(10.0 * scale),
            crate::ui_layout::Item::new(
                Size::Pixels(54.0 * scale),
                Size::Pixels((row.height - 4.0 * scale).max(10.0 * scale)),
            ),
            crate::ui_layout::Item::width(2.0 * scale),
            crate::ui_layout::Item::new(
                Size::Fill,
                Size::Pixels((row.height - 4.0 * scale).max(10.0 * scale)),
            ),
            crate::ui_layout::Item::width(10.0 * scale),
        ],
        0.0,
        0.0,
        kama_ui::Align::Center,
    );
    (parts[1], parts[3])
}

pub(super) fn graph_scalar_input_port(card_rect: Rect, card: &GraphCard, index: usize) -> Rect {
    let scale = graph_card_scale(card_rect);
    let row = graph_property_row_rect(card_rect, card, index);
    let size = (8.0 * scale).max(4.0);
    Rect::new(
        card_rect.x - size * 0.5,
        row.y + row.height * 0.5 - size * 0.5,
        size,
        size,
    )
}

pub(super) fn graph_scalar_input_point(
    card_rect: Rect,
    card: &GraphCard,
    index: usize,
) -> [f32; 2] {
    let port = graph_scalar_input_port(card_rect, card, index);
    [port.x + port.width * 0.5, port.y + port.height * 0.5]
}

pub(super) fn graph_value_input_port(card: Rect, index: usize) -> Rect {
    let s = graph_card_scale(card);
    let size = (8.0 * s).max(4.0);
    Rect::new(
        card.x - size * 0.5,
        card.y + (GRAPH_CARD_BASE_H + index as f32 * GRAPH_INPUT_H + 4.0) * s,
        size,
        size,
    )
}

pub(super) fn graph_value_input_point(card: Rect, index: usize) -> [f32; 2] {
    let port = graph_value_input_port(card, index);
    [port.x + port.width * 0.5, port.y + port.height * 0.5]
}

pub(super) fn graph_value_input_label_rect(card: Rect, index: usize) -> Rect {
    let scale = graph_card_scale(card);
    let row = graph_value_input_row(card, index);
    crate::ui_layout::row(
        row,
        &[
            crate::ui_layout::Item::width(9.0 * scale),
            crate::ui_layout::Item::new(Size::Pixels(42.0 * scale), Size::Pixels(16.0 * scale)),
            crate::ui_layout::Item::fill(),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    )[1]
}

pub(super) fn graph_value_input_area(card: Rect, index: usize, linkable: bool) -> Rect {
    let scale = graph_card_scale(card);
    let reserve = if linkable { 20.0 * scale } else { 0.0 };
    let row = graph_value_input_row(card, index);
    crate::ui_layout::row(
        row,
        &[
            crate::ui_layout::Item::width(53.0 * scale),
            crate::ui_layout::Item::new(Size::Fill, Size::Pixels(16.0 * scale)),
            crate::ui_layout::Item::width((9.0 * scale) + reserve),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    )[1]
}

pub(super) fn graph_value_input_link_rect(card: Rect, index: usize) -> Rect {
    let scale = graph_card_scale(card);
    let row = graph_value_input_row(card, index);
    crate::ui_layout::row(
        row,
        &[
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::new(Size::Pixels(18.0 * scale), Size::Pixels(16.0 * scale)),
            crate::ui_layout::Item::width(9.0 * scale),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    )[1]
}

pub(super) fn graph_component_rect(area: Rect, scale: f32, component: usize, count: usize) -> Rect {
    let count = count.max(1);
    let items = std::iter::repeat_n(crate::ui_layout::Item::fill(), count).collect::<Vec<_>>();
    crate::ui_layout::row(area, &items, 2.0 * scale, 0.0, kama_ui::Align::Start)
        [component.min(count - 1)]
}

pub(super) fn graph_value_input_component_rect(
    card: Rect,
    index: usize,
    component: usize,
    component_count: usize,
    linkable: bool,
) -> Rect {
    graph_component_rect(
        graph_value_input_area(card, index, linkable),
        graph_card_scale(card),
        component,
        component_count,
    )
}

pub(super) fn graph_value_swatch_rect(card: Rect) -> Rect {
    let scale = graph_card_scale(card);
    let (_, rows) = crate::ui_layout::fit_column_at(
        card,
        [card.x, card.y + 29.0 * scale],
        card.width,
        &[crate::ui_layout::Item::height(18.0 * scale)],
        0.0,
        0.0,
    );
    crate::ui_layout::row(
        rows[0],
        &[
            crate::ui_layout::Item::width(9.0 * scale),
            crate::ui_layout::Item::width(36.0 * scale),
            crate::ui_layout::Item::fill(),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    )[1]
}

pub(super) fn graph_value_component_rect(card: Rect, component: usize, linkable: bool) -> Rect {
    let scale = graph_card_scale(card);
    let reserve = if linkable { 22.0 * scale } else { 0.0 };
    let row = graph_value_input_row(card, component);
    crate::ui_layout::row(
        row,
        &[
            crate::ui_layout::Item::width(9.0 * scale),
            crate::ui_layout::Item::new(Size::Fill, Size::Pixels(16.0 * scale)),
            crate::ui_layout::Item::width(9.0 * scale + reserve),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    )[1]
}

pub(super) fn graph_value_link_rect(card: Rect) -> Rect {
    let scale = graph_card_scale(card);
    let row = graph_value_input_row(card, 0);
    crate::ui_layout::row(
        row,
        &[
            crate::ui_layout::Item::fill(),
            crate::ui_layout::Item::new(Size::Pixels(18.0 * scale), Size::Pixels(16.0 * scale)),
            crate::ui_layout::Item::width(9.0 * scale),
        ],
        0.0,
        0.0,
        kama_ui::Align::Start,
    )[1]
}
