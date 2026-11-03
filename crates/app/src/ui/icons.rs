//! Small, embedded outline glyphs. Rendering never reads the filesystem.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString, Svg, prelude::*, px, svg};

use super::theme::Theme;

#[derive(Clone, Copy)]
pub enum Icon {
    Sidebar,
    Account,
    Channels,
    Downloads,
    Library,
    Settings,
    Minimize,
    Maximize,
    Close,
}

impl Icon {
    const ALL: &[Self] = &[
        Self::Sidebar,
        Self::Account,
        Self::Channels,
        Self::Downloads,
        Self::Library,
        Self::Settings,
        Self::Minimize,
        Self::Maximize,
        Self::Close,
    ];

    fn path(self) -> &'static str {
        match self {
            Self::Sidebar => "icons/sidebar.svg",
            Self::Account => "icons/account.svg",
            Self::Channels => "icons/channels.svg",
            Self::Downloads => "icons/downloads.svg",
            Self::Library => "icons/library.svg",
            Self::Settings => "icons/settings.svg",
            Self::Minimize => "icons/minimize.svg",
            Self::Maximize => "icons/maximize.svg",
            Self::Close => "icons/close.svg",
        }
    }

    fn svg(self) -> &'static [u8] {
        match self {
            Self::Sidebar => br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="3" width="18" height="18" rx="3"/><path d="M9 3v18"/></svg>"#,
            Self::Account => br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9"/><circle cx="12" cy="9" r="3"/><path d="M6 18.5c.5-2.5 2.5-4 6-4s5.5 1.5 6 4"/></svg>"#,
            Self::Channels => br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z"/></svg>"#,
            Self::Downloads => br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3v12m-5-5 5 5 5-5M4 15v4a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-4"/></svg>"#,
            Self::Library => br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="5" height="16" rx="1"/><rect x="8" y="4" width="5" height="16" rx="1"/><path d="m15 4 5-1 3 16-5 1ZM3 15h10"/></svg>"#,
            Self::Settings => br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h3m4 0h11M3 12h11m4 0h3M3 18h3m4 0h11"/><circle cx="8" cy="6" r="2"/><circle cx="16" cy="12" r="2"/><circle cx="8" cy="18" r="2"/></svg>"#,
            Self::Minimize => br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.5"><path d="M5 12h14"/></svg>"#,
            Self::Maximize => br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.5"><rect x="5" y="5" width="14" height="14" rx="1"/></svg>"#,
            Self::Close => br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.5" stroke-linecap="round"><path d="m6 6 12 12M18 6 6 18"/></svg>"#,
        }
    }

    pub fn element(self) -> Svg {
        // Decorative when used beside a label; the parent control supplies its
        // accessible name. No extra image role/node is attached to the glyph.
        svg()
            .path(self.path())
            .size(px(18.))
            .flex_shrink_0()
            .text_color(Theme::dark().muted)
    }
}

pub struct IconAssets;

impl AssetSource for IconAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(Icon::ALL
            .iter()
            .find(|icon| icon.path() == path)
            .map(|icon| Cow::Borrowed(icon.svg())))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(Icon::ALL
            .iter()
            .filter(|icon| icon.path().starts_with(path))
            .map(|icon| SharedString::from(icon.path()))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_glyph_is_embedded_and_unknown_assets_are_not_loaded() -> anyhow::Result<()> {
        for icon in Icon::ALL {
            assert!(IconAssets.load(icon.path())?.is_some());
        }
        assert!(IconAssets.load("missing.svg")?.is_none());
        assert_eq!(IconAssets.list("icons/")?.len(), Icon::ALL.len());
        Ok(())
    }
}
