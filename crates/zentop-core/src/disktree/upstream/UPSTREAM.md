# Upstream components

Source: https://github.com/tobi/disktree
Revision: `9b18b272ca3c5a145f6ff58eaf89dc4d311eeaea`
License: MIT, copyright (c) 2026 Tobi Lütke.
The complete notice is retained in [`licenses/disktree-MIT.md`](../../../../../licenses/disktree-MIT.md).

This core directory copies only these components (the GUI presentation
components have separate provenance in `zentop-gui/src/ui/disktree/upstream/UPSTREAM.md`):

- `treemap.rs`: `Rect` and its required geometry methods, `squarify`,
  `worst_ratio`, and the three squarification tests. The upstream Node-based
  layout, filter integration and UI types are excluded. Zentop's adapter supplies
  its flat node IDs, nesting bands, drawing limits and TUI cell aspect ratio.
- `classify.rs`: Category/Reclaim types, their labels, and the pure name/sibling
  lookup functions (`lowered`, `category_of_name`, `reclaim_of`). The upstream
  recursive Node classifier and Rayon traversal are excluded. Zentop applies
  the lookups to its own scan records and keeps worktrees as review-only hints.
- `size.rs`: the complete, dependency-free formatting/share component and tests.

Extraction retains the function bodies; Rustfmt applies workspace formatting.
No upstream scanner, desktop shell, remote operations, deletion or export code
is included. Runtime code is integrated through the parent DiskTree modules;
these are used components, not unused reference copies.
