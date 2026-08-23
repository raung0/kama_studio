use crate::Color;

#[derive(Clone, Copy)]
pub struct Style {
    pub text: Color,
    pub muted: Color,
    pub accent: Color,
    pub accent_text: Color,
    pub control: Color,
    pub focused: Color,
    pub border: Color,
    pub radius_sm: f32,
    pub radius_md: f32,

    pub text_scale: f32,

    pub ui_scale: f32,
}

impl Style {
    pub fn with_scale(mut self, scale: f32) -> Self {
        let scale = scale.clamp(0.25, 4.0);
        self.text_scale = scale;
        self.ui_scale = scale;
        self.radius_sm *= scale;
        self.radius_md *= scale;
        self
    }
}
