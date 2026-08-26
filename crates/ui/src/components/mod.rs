mod accordion;
mod button;
mod color_button;
mod color_picker;
mod combobox;
mod knob;
mod label;
mod number_input;
mod primitives;
mod progress_bar;
mod slider;
mod spin_input;
mod style;
mod text_edit;
mod toggle_button;
mod vertical_slider;

pub use accordion::{Accordion, AccordionContent};
pub use button::Button;
pub use color_button::ColorButton;
pub use color_picker::ColorPicker;
pub use combobox::{ComboBox, ComboBoxOpenDirection};
pub use knob::Knob;
pub use label::Label;
pub use number_input::NumberInput;
pub use primitives::{
    ColumnUi, ColumnUiProps, HSpacerUi, HSpacerUiProps, IconUi, IconUiProps, RowUi, RowUiProps,
    VSpacerUi, VSpacerUiProps,
};
pub use progress_bar::ProgressBar;
pub use slider::Slider;
pub use spin_input::SpinInput;
pub use style::Style;
pub use text_edit::{EditResponse, TextEdit};
pub use toggle_button::ToggleButton;
pub use vertical_slider::VerticalSlider;

fn ease(value: &mut f32, target: f32, speed: f32, dt: f32) {
    *value = (target - *value).mul_add(1.0 - (-speed * dt).exp(), *value);
    if (*value - target).abs() < 0.001 {
        *value = target;
    }
}
