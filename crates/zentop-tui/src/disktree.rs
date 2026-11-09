//! DiskTree's terminal projection uses the same scanner/layout as the GUI.
use crate::{app::Effect, theme::Theme, ui::plain};
use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use std::{path::PathBuf, sync::atomic::Ordering};
use zentop_core::disktree::{self as disk, Category, Metric, Review, Session, Tile};

#[derive(Default)]
pub struct DiskTree {
    pub active: bool,
    pub session: Session,
    pub tiles: Vec<Tile>,
    editing: Option<bool>,
    input: String,
    help: bool,
    review: bool,
    review_index: usize,
}
fn rgb(rgb: [u8; 3]) -> Color {
    Color::Rgb(rgb[0], rgb[1], rgb[2])
}
impl DiskTree {
    pub fn open(&mut self, path: PathBuf) {
        self.active = true;
        self.session.start(path);
    }
    pub fn tick(&mut self) -> bool {
        let changed = self.session.poll();
        changed || self.session.job.is_some()
    }
    pub fn key(&mut self, key: KeyEvent) -> Effect {
        if let Some(path) = self.editing {
            match key.code {
                KeyCode::Esc => self.editing = None,
                KeyCode::Backspace => {
                    self.input.pop();
                }
                KeyCode::Enter => {
                    if path {
                        self.session.start(PathBuf::from(&self.input));
                    } else if let Some(view) = &mut self.session.view {
                        view.options.filter = self.input.clone();
                        view.rebuild();
                    }
                    self.editing = None;
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.input.clear()
                }
                KeyCode::Char(c)
                    if !c.is_control()
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.input.push(c)
                }
                _ => {}
            }
            return Effect::None;
        }
        if self.help {
            self.help = false;
            return Effect::None;
        }
        if key.code == KeyCode::Char('q') {
            return Effect::Quit;
        }
        if key.code == KeyCode::Char('?') {
            self.help = true;
            return Effect::None;
        }
        if key.code == KeyCode::Char('v')
            || (key.code == KeyCode::Char('o') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            self.input = self
                .session
                .path
                .as_ref()
                .map_or_else(String::new, |p| p.to_string_lossy().into_owned());
            self.editing = Some(true);
            return Effect::None;
        }
        if key.code == KeyCode::Char('r') {
            if let Some(path) = self.session.path.clone() {
                self.session.start(path);
            }
            return Effect::None;
        }
        if key.code == KeyCode::Esc && self.session.job.is_some() {
            self.session.cancel();
            return Effect::None;
        }
        if key.code == KeyCode::Char('M') {
            self.active = false;
            return Effect::None;
        }
        let Some(view) = &mut self.session.view else {
            if key.code == KeyCode::Esc {
                self.active = false;
            }
            return Effect::None;
        };
        if self.review {
            let mut marked: Vec<_> = view.marked.iter().copied().collect();
            marked.sort_unstable();
            match key.code {
                KeyCode::Esc | KeyCode::Char('c') => self.review = false,
                KeyCode::Up | KeyCode::Char('k') => {
                    self.review_index = self.review_index.saturating_sub(1)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.review_index = (self.review_index + 1).min(marked.len().saturating_sub(1))
                }
                KeyCode::Char(' ') | KeyCode::Char('x') => {
                    if let Some(id) = marked.get(self.review_index) {
                        view.marked.remove(id);
                    }
                }
                KeyCode::Enter => {
                    if let Some(id) = marked.get(self.review_index) {
                        view.selected = *id;
                        view.enter();
                        self.review = false;
                    }
                }
                KeyCode::Char('!') => view.marked.clear(),
                _ => {}
            }
            return Effect::None;
        }
        match key.code {
            KeyCode::Char('/') => {
                self.input = view.options.filter.clone();
                self.editing = Some(false);
            }
            KeyCode::Char('t') => {
                view.options.metric = view.options.metric.next();
                view.rebuild();
            }
            KeyCode::Char('i') => {
                view.options.hidden = !view.options.hidden;
                view.rebuild();
            }
            KeyCode::Char('d') | KeyCode::Char('a') => {
                view.options.apparent = !view.options.apparent;
                view.rebuild();
            }
            KeyCode::Char('[') => view.options.depth = view.options.depth.saturating_sub(1).max(1),
            KeyCode::Char(']') => view.options.depth = (view.options.depth + 1).min(8),
            KeyCode::Char(' ') | KeyCode::Char('x') => view.mark(),
            KeyCode::Enter => view.enter(),
            KeyCode::Backspace | KeyCode::Esc => {
                if !view.options.filter.is_empty() {
                    view.options.filter.clear();
                    view.rebuild();
                } else if view.zoom == 0 {
                    self.active = false;
                } else {
                    view.up();
                }
            }
            KeyCode::Char('0') => view.reset(),
            KeyCode::Left | KeyCode::Char('h') => view.move_selection(&self.tiles, -1., 0.),
            KeyCode::Right | KeyCode::Char('l') => view.move_selection(&self.tiles, 1., 0.),
            KeyCode::Up | KeyCode::Char('k') => view.move_selection(&self.tiles, 0., -1.),
            KeyCode::Down | KeyCode::Char('j') => view.move_selection(&self.tiles, 0., 1.),
            KeyCode::Tab => {
                let parent = view.tree.nodes[view.selected].parent.unwrap_or(view.zoom);
                let siblings: Vec<_> = self
                    .tiles
                    .iter()
                    .filter(|t| view.tree.nodes[t.id].parent == Some(parent))
                    .collect();
                if !siblings.is_empty() {
                    let index = siblings
                        .iter()
                        .position(|t| t.id == view.selected)
                        .map_or(0, |i| (i + 1) % siblings.len());
                    view.selected = siblings[index].id;
                }
            }
            KeyCode::Char('c') => {
                self.review = true;
                self.review_index = 0;
            }
            KeyCode::Char('w') => {
                let candidates = view.review();
                if !candidates.is_empty() {
                    let next = candidates
                        .iter()
                        .position(|i| *i == view.selected)
                        .map_or(0, |i| (i + 1) % candidates.len());
                    view.selected = candidates[next];
                }
            }
            _ => {}
        }
        Effect::None
    }

    pub fn draw(&mut self, frame: &mut Frame, theme: &Theme) {
        let palette = theme.disktree_palette();
        let now = std::time::SystemTime::now();
        let area = frame.area();
        frame.render_widget(
            Block::default().style(Style::default().bg(theme.background).fg(theme.foreground)),
            area,
        );
        let [header, legend, body, footer] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .areas(area);
        let path = self
            .session
            .view
            .as_ref()
            .map(|v| v.tree.path(v.zoom))
            .or_else(|| self.session.path.clone());
        let title = format!(
            " ▦ DiskTree   {}",
            path.as_ref().map_or_else(
                || "Choose a directory [v]".into(),
                |p| plain(&p.display().to_string())
            )
        );
        let controls = self.session.view.as_ref().map_or_else(String::new, |v| {
            format!(
                "{} [t] · {} hidden [i] · {} [d] · Depth {} [[]]",
                v.options.metric.label(),
                if v.options.hidden { "✓" } else { "□" },
                if v.options.apparent || !v.tree.allocated_supported {
                    "Apparent size"
                } else {
                    "Allocated size"
                },
                v.options.depth
            )
        });
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(title, Style::default().add_modifier(Modifier::BOLD)),
                Line::styled(controls, Style::default().fg(theme.muted_foreground)),
            ]),
            header,
        );
        let legend_spans: Vec<_> = if self
            .session
            .view
            .as_ref()
            .is_some_and(|v| v.options.metric == Metric::Age)
        {
            std::iter::once(Span::raw("Age: "))
                .chain(
                    disk::palette::AGE_LABELS
                        .into_iter()
                        .enumerate()
                        .map(|(i, label)| {
                            Span::styled(
                                format!("{label}  "),
                                Style::default().fg(rgb(palette.age(i))),
                            )
                        }),
                )
                .collect()
        } else {
            Category::ALL
                .into_iter()
                .flat_map(|c| {
                    [
                        Span::styled("■ ", Style::default().fg(rgb(palette.category(c)))),
                        Span::raw(format!("{}  ", c.label())),
                    ]
                })
                .collect()
        };
        frame.render_widget(Paragraph::new(Line::from(legend_spans)), legend);
        let side_width = if area.width >= 110 {
            34
        } else if area.width >= 78 {
            28
        } else {
            0
        };
        let [map, sidebar] =
            Layout::horizontal([Constraint::Min(1), Constraint::Length(side_width)]).areas(body);
        if let Some(view) = &self.session.view {
            // Terminal cells are approximately twice as tall as they are wide.
            self.tiles = view.tiles(map.width as f64, map.height as f64 * 2., 2., 1.);
            for tile in &mut self.tiles {
                tile.rect.y /= 2.;
                tile.rect.h /= 2.;
            }
            for tile in &self.tiles {
                let left = tile.rect.x.floor() as u16;
                let top = tile.rect.y.floor() as u16;
                let right = (tile.rect.x + tile.rect.w).floor() as u16;
                let bottom = (tile.rect.y + tile.rect.h).floor() as u16;
                let rect = Rect::new(
                    map.x + left.min(map.width),
                    map.y + top.min(map.height),
                    right
                        .saturating_sub(left)
                        .min(map.width.saturating_sub(left)),
                    bottom
                        .saturating_sub(top)
                        .min(map.height.saturating_sub(top)),
                );
                if rect.width == 0 || rect.height == 0 {
                    continue;
                }
                let node = &view.tree.nodes[tile.id];
                let selected = tile.id == view.selected;
                let mut parent = Some(tile.id);
                let mut marked = false;
                while let Some(id) = parent {
                    marked |= view.marked.contains(&id);
                    parent = view.tree.nodes[id].parent;
                }
                let color = if marked {
                    palette.danger
                } else if view.options.metric == Metric::Age {
                    palette.age(disk::palette::age_bucket(node.modified, now))
                } else {
                    palette.category(node.category)
                };
                let background = rgb(if marked {
                    palette.marked_fill()
                } else {
                    palette.fill(color, tile.depth)
                });
                let hatched = view.hatched(tile.id);
                let pattern: Vec<Line> = (0..rect.height)
                    .map(|y| {
                        Line::from(
                            (0..rect.width)
                                .map(|x| {
                                    if hatched && (x + y) % 5 == 0 {
                                        '╱'
                                    } else {
                                        ' '
                                    }
                                })
                                .collect::<String>(),
                        )
                    })
                    .collect();
                let title = format!(
                    "{}{} {}",
                    if marked { "◆ " } else { "" },
                    plain(&node.name.to_string_lossy()),
                    disk::human_bytes_short(view.bytes[tile.id])
                );
                let border = Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(if selected {
                        theme.primary
                    } else {
                        rgb(color)
                    }))
                    .title(Line::styled(
                        title,
                        Style::default()
                            .fg(theme.foreground)
                            .add_modifier(if selected {
                                Modifier::BOLD
                            } else {
                                Modifier::empty()
                            }),
                    ));
                let paragraph = Paragraph::new(pattern)
                    .style(Style::default().bg(background).fg(theme.muted_foreground));
                if rect.width >= 6 && rect.height >= 2 {
                    frame.render_widget(paragraph.block(border), rect);
                } else {
                    frame.render_widget(paragraph, rect);
                }
            }
            if self.tiles.is_empty() {
                frame.render_widget(Paragraph::new("No visible entries with nonzero weight. Try Files mode [t], hidden files [i], or clear the filter [Esc].").wrap(Wrap{trim:false}),map);
            }
            if side_width > 0 {
                self.sidebar(frame, sidebar, theme);
            }
        } else {
            frame.render_widget(Paragraph::new("Choose a folder [v] to scan its disk usage.\n\nScans are read-only; file contents are not read.").wrap(Wrap{trim:false}).style(Style::default().fg(theme.muted_foreground)),map);
        }
        let status = if let Some(job) = &self.session.job {
            format!(
                "Scanning… {} entries · {} · Esc cancel",
                job.progress.entries.load(Ordering::Relaxed),
                disk::format_size(job.progress.bytes.load(Ordering::Relaxed))
            )
        } else if let Some(error) = &self.session.error {
            plain(error)
        } else if let Some(view) = &self.session.view {
            format!(
                "{} · {} files · {} dirs · {} skipped{} · {:.1}s",
                disk::format_size(view.bytes[view.zoom]),
                disk::human_count(view.files[view.zoom]),
                disk::human_count(view.dirs[view.zoom]),
                view.tree.skipped,
                if view.tree.truncated {
                    " · PARTIAL (scan limit)"
                } else {
                    ""
                },
                view.tree.elapsed.as_secs_f64()
            )
        } else {
            String::new()
        };
        frame.render_widget(Paragraph::new(vec![Line::from("Space mark  Enter open  ⌫ up  hjkl move  / filter  c review  w suggestions  r rescan  M monitor  ? keys"),Line::styled(status,Style::default().fg(theme.muted_foreground))]),footer);
        if self.editing.is_some() || self.help || self.review {
            self.overlay(frame, theme);
        }
    }

    fn sidebar(&self, frame: &mut Frame, rect: Rect, theme: &Theme) {
        let Some(view) = &self.session.view else {
            return;
        };
        let node = &view.tree.nodes[view.selected];
        let share = if view.bytes[view.zoom] > 0 {
            disk::share(view.bytes[view.selected], view.bytes[view.zoom])
        } else {
            0.
        };
        let mut lines = vec![
            Line::styled("SELECTION", Style::default().fg(theme.muted_foreground)),
            Line::from(""),
            Line::styled(
                plain(&node.name.to_string_lossy()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Line::from(plain(&view.tree.path(view.selected).display().to_string())),
            Line::from(""),
            Line::styled(
                disk::format_size(view.bytes[view.selected]),
                Style::default()
                    .fg(theme.primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::from(format!(
                "{share:.1}% of view · {} files",
                view.files[view.selected]
            )),
            Line::from(format!("Last write: {}", disk::age(node.modified))),
            Line::from(format!(
                "Kind: {}{}",
                node.category.label(),
                if node.symlink { " · symlink" } else { "" }
            )),
            Line::from(""),
        ];
        if let Some(issue) = &node.issue {
            lines.push(Line::from(plain(issue)));
        }
        lines.push(Line::styled(
            "WORTH A LOOK [w]",
            Style::default().fg(theme.muted_foreground),
        ));
        for id in view.review().into_iter().take(5) {
            let n = &view.tree.nodes[id];
            lines.push(Line::styled(
                format!(
                    "{} {}",
                    plain(&n.name.to_string_lossy()),
                    disk::format_size(view.bytes[id])
                ),
                Style::default().fg(rgb(theme.disktree_palette().category(n.category))),
            ));
            lines.push(Line::from(
                n.review.map_or("Review before removing", Review::hint),
            ));
        }
        lines.extend([
            Line::from(""),
            Line::styled(
                format!(
                    "MARKED {} · {} [c]",
                    view.marked.len(),
                    disk::format_size(view.marked_bytes())
                ),
                Style::default().fg(theme.primary),
            ),
            Line::from("Read-only review; no deletion actions."),
        ]);
        if let Some(volume) = &view.tree.volume {
            lines.extend([
                Line::from(""),
                Line::styled(
                    format!("DISK {}", plain(&volume.mount.display().to_string())),
                    Style::default().fg(theme.muted_foreground),
                ),
                Line::from(format!("{} free", disk::format_size(volume.available))),
                Line::from(format!("{} total", disk::format_size(volume.total))),
                Line::from(disk::share_bar(
                    volume.total.saturating_sub(volume.available),
                    volume.total,
                    24,
                )),
            ]);
        }
        let border = Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(theme.border));
        frame.render_widget(
            Paragraph::new(lines)
                .block(border)
                .wrap(Wrap { trim: false })
                .style(Style::default().bg(theme.card).fg(theme.foreground)),
            rect,
        );
    }
    fn overlay(&self, frame: &mut Frame, theme: &Theme) {
        let area = frame.area();
        let rect = Rect::new(
            area.x + area.width / 10,
            area.y + area.height / 8,
            area.width * 8 / 10,
            area.height * 3 / 4,
        );
        frame.render_widget(Clear, rect);
        let (title, lines) = if let Some(path) = self.editing {
            (
                if path {
                    "Scan directory"
                } else {
                    "Filter by name"
                },
                vec![
                    format!("{}▏", plain(&self.input)),
                    "Enter apply · Ctrl+U clear · Esc cancel".into(),
                ],
            )
        } else if self.review {
            let mut lines = vec![
                "Read-only marked paths · Space unmark · ! clear · Enter open · Esc back".into(),
                String::new(),
            ];
            if let Some(view) = &self.session.view {
                let mut ids: Vec<_> = view.marked.iter().copied().collect();
                ids.sort_unstable();
                if ids.is_empty() {
                    lines.push("Nothing marked. Select a tile and press Space.".into());
                }
                for (i, id) in ids.iter().enumerate() {
                    lines.push(format!(
                        "{}{} · {}",
                        if i == self.review_index { "› " } else { "  " },
                        plain(&view.tree.path(*id).display().to_string()),
                        disk::format_size(
                            if view.options.apparent || !view.tree.allocated_supported {
                                view.tree.nodes[*id].apparent
                            } else {
                                view.tree.nodes[*id].allocated
                            }
                        )
                    ));
                }
            }
            ("Review marked items", lines)
        } else {
            (
                "DiskTree keys",
                vec![
                    "Inspired by tobi/disktree · Read-only disk usage exploration",
                    "Enter open selected directory · Backspace/Esc up",
                    "Arrows / hjkl move · Tab next sibling · 0 root",
                    "Space / x mark or unmark · c review marked paths",
                    "w select Worth a look suggestion",
                    "t Size / Files / Age (age changes color, not area)",
                    "d / a allocated / apparent bytes · i hidden files",
                    "[ / ] less / more depth · / filter · r rescan",
                    "v / Ctrl+O choose scan directory · Esc cancel scan",
                    "M return to monitor · q / Ctrl+C quit",
                    "",
                    "Symlinks are not followed. Unix hard links count once.",
                    "Mount/depth/entry limits and unreadable paths are reported.",
                    "Allocated blocks may overlap on copy-on-write filesystems.",
                    "Marks are for review; this feature never removes files.",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            )
        };
        let text: Vec<_> = lines.into_iter().map(Line::from).collect();
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .block(
                    Block::bordered()
                        .title(title)
                        .border_style(Style::default().fg(theme.primary)),
                )
                .style(Style::default().bg(theme.card).fg(theme.foreground)),
            rect,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use disk::{Node, Tree, View, Volume};
    use ratatui::{backend::TestBackend, Terminal};
    use std::time::{Duration, SystemTime};
    fn fixture() -> DiskTree {
        let mut nodes = Vec::new();
        fn add(
            nodes: &mut Vec<Node>,
            parent: Option<usize>,
            name: &str,
            size: u64,
            category: Category,
            review: Option<Review>,
        ) -> usize {
            let id = nodes.len();
            let is_dir = size == 0;
            nodes.push(Node {
                name: name.into(),
                parent,
                children: Vec::new(),
                is_dir,
                symlink: false,
                hidden: name.starts_with('.'),
                apparent: size,
                allocated: size,
                files: u64::from(!is_dir),
                dirs: u64::from(is_dir && parent.is_some()),
                modified: Some(SystemTime::now() - Duration::from_secs(1920)),
                category,
                review,
                issue: None,
                own_files: u64::from(!is_dir),
                own_apparent: size,
                own_allocated: size,
            });
            if let Some(p) = parent {
                nodes[p].children.push(id);
            }
            id
        }
        let root = add(&mut nodes, None, "home", 0, Category::Code, None);
        let gib = 1 << 30;
        for (name, category, size, child, review) in [
            ("src", Category::Code, 185, "projects", None),
            (".local", Category::Toolchain, 120, "share", None),
            ("Sync", Category::Synced, 96, "Telemetry", None),
            (
                ".codex",
                Category::AgentScratch,
                96,
                "worktrees",
                Some(Review::Worktree),
            ),
            ("world", Category::Git, 92, ".git", None),
            ("Documents", Category::Documents, 75, "collection", None),
            (".cache", Category::Cache, 69, "store", Some(Review::Cache)),
            (
                ".microsandbox",
                Category::AgentScratch,
                57,
                "layers",
                Some(Review::Cache),
            ),
            ("models", Category::Media, 35, "weights", None),
        ] {
            let dir = add(
                &mut nodes,
                Some(root),
                name,
                0,
                category,
                if name == ".cache" { review } else { None },
            );
            let child = add(
                &mut nodes,
                Some(dir),
                child,
                0,
                category,
                if name == ".cache" { None } else { review },
            );
            add(&mut nodes, Some(child), "data", size * gib, category, None);
        }
        for id in (1..nodes.len()).rev() {
            let p = nodes[id].parent.unwrap();
            nodes[p].apparent += nodes[id].apparent;
            nodes[p].allocated += nodes[id].allocated;
            nodes[p].files += nodes[id].files;
            nodes[p].dirs += nodes[id].dirs;
        }
        let tree = Tree {
            root: "/home/example".into(),
            nodes,
            volume: Some(Volume {
                mount: "/".into(),
                total: 952 * gib,
                available: 107 * gib,
            }),
            skipped: 0,
            errors: vec![],
            truncated: false,
            allocated_supported: true,
            elapsed: Duration::from_millis(50),
        };
        DiskTree {
            active: true,
            session: Session {
                view: Some(View::new(tree)),
                ..Session::default()
            },
            ..DiskTree::default()
        }
    }
    #[test]
    fn every_theme_keeps_tile_labels_readable_and_uses_shared_age_bands() {
        use disk::palette::{contrast_ratio, AGE_LABELS};
        for theme in crate::theme::load().unwrap() {
            let palette = theme.disktree_palette();
            for accent in Category::ALL
                .map(|c| palette.category(c))
                .into_iter()
                .chain((0..AGE_LABELS.len()).map(|i| palette.age(i)))
            {
                for depth in 0..8 {
                    assert!(
                        contrast_ratio(palette.foreground, palette.fill(accent, depth)) >= 4.5,
                        "{} depth {depth}",
                        theme.name
                    );
                }
            }
            assert!(
                contrast_ratio(palette.foreground, palette.marked_fill()) >= 4.5,
                "{} marks",
                theme.name
            );
            let mut disk = fixture();
            disk.session.view.as_mut().unwrap().options.metric = Metric::Age;
            let mut terminal = Terminal::new(TestBackend::new(160, 54)).unwrap();
            terminal.draw(|f| disk.draw(f, &theme)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            for label in AGE_LABELS {
                assert!(text.contains(label), "{} missing {label}", theme.name);
            }
            assert!(
                terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .any(|c| c.bg == rgb(palette.fill(palette.age(0), 2))),
                "{} missing themed recent tile",
                theme.name
            );
        }
    }

    #[test]
    fn treemap_and_review_render_at_all_sizes() {
        let mut disk = fixture();
        let theme = crate::theme::load().unwrap().remove(0);
        for (w, h) in [(160, 54), (100, 36), (80, 24), (40, 12), (1, 1), (0, 0)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| disk.draw(f, &theme)).unwrap();
            if w == 160 {
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect();
                for title in ["DiskTree", "SELECTION", "WORTH A LOOK", "MARKED", "DISK"] {
                    assert!(text.contains(title), "missing {title}");
                }
                if let Ok(directory) = std::env::var("ZENTOP_TUI_CAPTURE") {
                    std::fs::create_dir_all(&directory).unwrap();
                    let cells:Vec<_>=terminal.backend().buffer().content.iter().map(|c|serde_json::json!({"text":c.symbol(),"fg":format!("{:?}",c.fg),"bg":format!("{:?}",c.bg)})).collect();
                    std::fs::write(
                        std::path::Path::new(&directory).join("disktree.json"),
                        serde_json::to_vec(
                            &serde_json::json!({"width":w,"height":h,"cells":cells}),
                        )
                        .unwrap(),
                    )
                    .unwrap();
                }
            }
            disk.review = true;
            terminal.draw(|f| disk.draw(f, &theme)).unwrap();
            disk.review = false;
            disk.help = true;
            terminal.draw(|f| disk.draw(f, &theme)).unwrap();
            disk.help = false;
            disk.editing = Some(true);
            terminal.draw(|f| disk.draw(f, &theme)).unwrap();
            disk.editing = None;
        }
    }
    #[test]
    fn controls_share_metric_hidden_marks_and_navigation_state() {
        let mut disk = fixture();
        let theme = crate::theme::load().unwrap().remove(0);
        let mut terminal = Terminal::new(TestBackend::new(160, 54)).unwrap();
        terminal.draw(|f| disk.draw(f, &theme)).unwrap();
        let mut key = |code| disk.key(KeyEvent::new(code, KeyModifiers::NONE));
        key(KeyCode::Tab);
        key(KeyCode::Char(' '));
        key(KeyCode::Char('c'));
        key(KeyCode::Esc);
        key(KeyCode::Enter);
        key(KeyCode::Char('t'));
        key(KeyCode::Char('i'));
        key(KeyCode::Char(']'));
        let view = disk.session.view.as_ref().unwrap();
        assert_ne!(view.zoom, 0);
        assert_eq!(view.options.metric, Metric::Files);
        assert!(!view.options.hidden);
        assert_eq!(view.options.depth, 5);
        assert_eq!(view.marked.len(), 1);
        let code = disk.key(KeyEvent::new(KeyCode::Char('M'), KeyModifiers::NONE));
        assert_eq!(code, Effect::None);
        assert!(!disk.active);
    }
}
