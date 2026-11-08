# Copied DiskTree UI components

Source: https://github.com/tobi/disktree
Revision: `9b18b272ca3c5a145f6ff58eaf89dc4d311eeaea`
License: MIT, copyright (c) 2026 Tobi Lütke.
The complete notice is retained in [`licenses/disktree-MIT.md`](../../../../../../licenses/disktree-MIT.md)
and embedded in Zentop's About dialog.

Only the required presentation components are included:

- `treemap_view.rs`: tile and label painter, decoration records, theme color
  cache, pixel snapping, clipped text shaping, hatching and selection outlines.
  The upstream `Mosaic`/`Label`/view projection records are adapted to Zentop's
  bounded flat-node layout, with narrow gaps between siblings and a canvas
  content mask. Hover and selection listeners remain in Zentop.
- `palette.rs`: category and age fills, accents, highlight, hatch, label colors,
  RGB mixing and their seven upstream tests. The unused action-text color helper
  is excluded. Tests use a local theme fixture.
- `widgets.rs`: breadcrumb, eyebrow, figure, size measure, bar, row, chip,
  percentage, meter, key hint, color swatch and hatch swatch components.
- `scale.rs`: only the spacing, type and size tokens these components use,
  extracted from upstream `ui.rs`.
- `panels.rs`: adapted selection identity/measure/stat grid, Worth a look
  candidate rows, marked review rows, legend and sidebar composition from
  `views.rs`. Paths, bytes, counts and listeners use Zentop's shared model;
  buttons use Zentop's installed GPUI component API. Review rows substitute
  a bounded percentage bar for upstream's glyph bar. The capacity section and
  key-hint arrangement are local composition of copied widgets.

`mod.rs` is a local theme/API adapter. It maps Zentop's active light/dark theme
onto the semantic color surface used by the copied components. Omarchy's theme
provider, fonts and desktop shell are not dependencies. GPUI import paths and
formatting are adapted to Zentop's current GPUI version.

Zentop owns the scanner, background jobs, navigation, toolbar/button event wiring,
filter application, native folder picker, accessibility controls and Ratatui
frontend. No upstream remote, deletion, trash, export, shell-command or git-status
stack is copied. Shared pure components have separate provenance in the core
crate's `disktree/upstream/UPSTREAM.md`.
