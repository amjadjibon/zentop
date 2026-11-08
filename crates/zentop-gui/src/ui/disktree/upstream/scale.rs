// Copyright (c) 2026 Tobi Lütke
// SPDX-License-Identifier: MIT
// Adapted from tobi/disktree 9b18b272ca3c5a145f6ff58eaf89dc4d311eeaea, crates/disktree-app/src/ui.rs.
// Full license: licenses/disktree-MIT.md. See UPSTREAM.md for adapters.

//! Required upstream spacing/type tokens.
use gpui_kit::Rems;
pub mod space {
    use super::Rems;
    pub const XXS: Rems = Rems(0.125);
    pub const XS: Rems = Rems(0.25);
    pub const SM: Rems = Rems(0.5);
    pub const MD: Rems = Rems(0.75);
    pub const LG: Rems = Rems(1.0);
    pub const XL: Rems = Rems(1.5);
}
pub mod text {
    use super::Rems;
    pub const CAPTION: Rems = Rems(0.6875);
    pub const BODY: Rems = Rems(0.75);
    pub const TITLE: Rems = Rems(0.875);
    pub const HEADING: Rems = Rems(1.125);
    pub const FIGURE: Rems = Rems(1.625);
    pub const DISPLAY: Rems = Rems(2.5);
}
pub mod size {
    use super::Rems;
    pub const METER: Rems = Rems(0.3125);
    pub const SWATCH: Rems = Rems(0.625);
    pub const ROW_BAR: Rems = Rems(5.5);
    pub const SHARE_LANE: Rems = Rems(6.0);
    pub const SIZE_LANE: Rems = Rems(5.0);
}
