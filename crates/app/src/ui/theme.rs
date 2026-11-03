use gpui::{Hsla, rgb};

pub const TOOLBAR_HEIGHT: f32 = 44.;
pub const SIDEBAR_WIDTH: f32 = 240.;

/// Neutral graphite surfaces, matching the supplied Delta reference.
pub struct Theme {
    pub canvas: Hsla,
    pub sidebar: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub accent: Hsla,
    pub selected: Hsla,
    pub border: Hsla,
    pub error: Hsla,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            canvas: rgb(0x1b1d1f).into(),
            sidebar: rgb(0x202224).into(),
            text: rgb(0xdcdde1).into(),
            muted: rgb(0xa6a8b2).into(),
            accent: rgb(0x91b5e6).into(),
            selected: rgb(0x2c2e32).into(),
            border: rgb(0x303236).into(),
            error: rgb(0xf29b9b).into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::Rgba;

    use super::*;

    fn luminance(color: Hsla) -> f32 {
        let color = Rgba::from(color);
        let linear = |value: f32| {
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
    }

    fn contrast(a: Hsla, b: Hsla) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn dark_text_and_focus_meet_contrast_requirements_on_every_surface() {
        let theme = Theme::dark();
        for surface in [theme.canvas, theme.sidebar, theme.selected] {
            for foreground in [theme.text, theme.muted, theme.error] {
                assert!(contrast(foreground, surface) >= 4.5);
            }
            assert!(contrast(theme.accent, surface) >= 3.);
        }
    }
}
