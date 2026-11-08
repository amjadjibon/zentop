//! Native, read-only disk usage explorer using components from tobi/disktree.
mod upstream;
use super::monitor;
use gpui_kit::component::{
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex, ActiveTheme, Disableable, Icon, Sizable, TitleBar,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    canvas, div, px, AppContext, Bounds, Context, Entity, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Render,
    StatefulInteractiveElement, Styled, Subscription, Task, Window,
};
use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::atomic::Ordering, time::Duration};
use upstream::treemap_view::{self, Filtered, Label, Mosaic, TileDeco};
use upstream::{palette, widgets, Theme};
use zentop_core::disktree::{self as disk, Metric, Session, Tile};

pub struct DiskTree {
    session: Session,
    filter: Entity<InputState>,
    focus: FocusHandle,
    hits: Rc<RefCell<(Bounds<Pixels>, Vec<Tile>)>>,
    reviewing: bool,
    hovered: Option<usize>,
    _subscriptions: Vec<Subscription>,
    _poll: Task<()>,
}
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

impl DiskTree {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter by name · Enter to apply"));
        let subscriptions = vec![
            cx.observe(&monitor::global(cx), |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &filter,
                window,
                |this, input, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        if let Some(view) = &mut this.session.view {
                            view.options.filter = input.read(cx).value().to_string();
                            view.rebuild();
                        }
                        this.focus.focus(window, cx);
                        cx.notify();
                    }
                },
            ),
        ];
        let poll = cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            if this
                .update(cx, |this, cx| {
                    if this.session.job.is_some() {
                        this.session.poll();
                        cx.notify();
                    }
                })
                .is_err()
            {
                break;
            }
        });
        Self {
            session: Session::default(),
            filter,
            focus: cx.focus_handle(),
            hits: Rc::new(RefCell::new((Bounds::default(), Vec::new()))),
            reviewing: false,
            hovered: None,
            _subscriptions: subscriptions,
            _poll: poll,
        }
    }
    fn choose(&mut self, cx: &mut Context<Self>) {
        let result = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose a folder to scan".into()),
        });
        cx.spawn(async move |this, cx| match result.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    let _ = this.update(cx, |this, cx| this.scan(path, cx));
                }
            }
            Ok(Err(error)) => {
                let _ = this.update(cx, |this, cx| {
                    this.session.error = Some(error.to_string());
                    cx.notify();
                });
            }
            _ => {}
        })
        .detach();
    }
    fn scan(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.session.start(path);
        self.reviewing = false;
        self.hovered = None;
        cx.notify();
    }
    fn change(&mut self, f: impl FnOnce(&mut disk::View), cx: &mut Context<Self>) {
        if let Some(view) = &mut self.session.view {
            f(view);
        }
        cx.notify();
    }
    fn key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) {
            return;
        }
        match key {
            "/" => self.filter.focus_handle(cx).focus(window, cx),
            "v" => self.choose(cx),
            "r" => {
                if let Some(path) = self.session.path.clone() {
                    self.scan(path, cx);
                }
            }
            "escape" if self.session.job.is_some() => {
                self.session.cancel();
                cx.notify();
            }
            "escape" if self.reviewing => {
                self.reviewing = false;
                cx.notify();
            }
            "c" => {
                self.reviewing = !self.reviewing;
                cx.notify();
            }
            "enter" => self.change(|v| v.enter(), cx),
            "backspace" | "escape" => self.change(|v| v.up(), cx),
            "space" | "x" => self.change(|v| v.mark(), cx),
            "0" => self.change(|v| v.reset(), cx),
            "t" => self.change(
                |v| {
                    v.options.metric = v.options.metric.next();
                    v.rebuild();
                },
                cx,
            ),
            "i" => self.change(
                |v| {
                    v.options.hidden = !v.options.hidden;
                    v.rebuild();
                },
                cx,
            ),
            "d" => self.change(
                |v| {
                    v.options.apparent = !v.options.apparent;
                    v.rebuild();
                },
                cx,
            ),
            "[" => self.change(
                |v| v.options.depth = v.options.depth.saturating_sub(1).max(1),
                cx,
            ),
            "]" => self.change(|v| v.options.depth = (v.options.depth + 1).min(8), cx),
            "left" | "h" | "right" | "l" | "up" | "k" | "down" | "j" => {
                let (dx, dy) = match key {
                    "left" | "h" => (-1., 0.),
                    "right" | "l" => (1., 0.),
                    "up" | "k" => (0., -1.),
                    _ => (0., 1.),
                };
                let tiles = self.hits.borrow().1.clone();
                self.change(|v| v.move_selection(&tiles, dx, dy), cx);
            }
            _ => {}
        }
    }
    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let options = self
            .session
            .view
            .as_ref()
            .map(|v| v.options.clone())
            .unwrap_or_default();
        let allocated_supported = self
            .session
            .view
            .as_ref()
            .is_none_or(|v| v.tree.allocated_supported);
        h_flex()
            .flex_shrink_0()
            .gap_3()
            .child(
                h_flex().gap_1().children(
                    [Metric::Size, Metric::Files, Metric::Age]
                        .into_iter()
                        .enumerate()
                        .map(|(i, metric)| {
                            Button::new(("disk-metric", i))
                                .small()
                                .label(metric.label())
                                .when(options.metric == metric, |b| b.primary())
                                .when(options.metric != metric, |b| b.ghost())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.change(
                                        |v| {
                                            v.options.metric = metric;
                                            v.rebuild();
                                        },
                                        cx,
                                    )
                                }))
                        }),
                ),
            )
            .child(
                Checkbox::new("disk-hidden")
                    .small()
                    .label("Hidden files")
                    .checked(options.hidden)
                    .on_click(cx.listener(|this, checked, _, cx| {
                        this.change(
                            |v| {
                                v.options.hidden = *checked;
                                v.rebuild();
                            },
                            cx,
                        )
                    })),
            )
            .child(
                Checkbox::new("disk-apparent")
                    .small()
                    .label("Apparent size")
                    .checked(options.apparent || !allocated_supported)
                    .disabled(!allocated_supported)
                    .on_click(cx.listener(|this, checked, _, cx| {
                        this.change(
                            |v| {
                                v.options.apparent = *checked;
                                v.rebuild();
                            },
                            cx,
                        )
                    })),
            )
            .child(
                Button::new("disk-depth-less")
                    .small()
                    .ghost()
                    .label("−")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.change(
                            |v| v.options.depth = v.options.depth.saturating_sub(1).max(1),
                            cx,
                        )
                    })),
            )
            .child(div().text_xs().child(format!("Depth {}", options.depth)))
            .child(
                Button::new("disk-depth-more")
                    .small()
                    .ghost()
                    .label("+")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.change(|v| v.options.depth = (v.options.depth + 1).min(8), cx)
                    })),
            )
    }
    fn header(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Leave room for native window controls and the current directory at smaller widths.
        let inline_controls = f32::from(window.viewport_size().width) >= 1280.;
        let scanning = self.session.job.is_some();
        let has_view = self.session.view.is_some();
        v_flex()
            .flex_shrink_0()
            .bg(cx.theme().title_bar)
            .child(
                TitleBar::new()
                    .h(px(42.))
                    .bg(cx.theme().title_bar)
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_3()
                            .px_3()
                            .child(
                                h_flex()
                                    .flex_shrink_0()
                                    .gap_2()
                                    .child(
                                        Icon::new(gpui_kit::assets::IconName::HardDrive)
                                            .small()
                                            .text_color(cx.theme().primary),
                                    )
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_sm()
                                            .child("DiskTree"),
                                    ),
                            )
                            .child(
                                Button::new("disk-choose")
                                    .ghost()
                                    .small()
                                    .icon(gpui_kit::assets::IconName::FolderOpen)
                                    .tooltip("Choose folder…")
                                    .on_click(cx.listener(|this, _, _, cx| this.choose(cx))),
                            )
                            .child(
                                Button::new("disk-up")
                                    .ghost()
                                    .small()
                                    .icon(gpui_kit::assets::IconName::ArrowUp)
                                    .tooltip("Up one directory")
                                    .disabled(!has_view)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.change(|v| v.up(), cx)),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .when(has_view, |d| d.child(self.breadcrumbs(cx)))
                                    .when(!has_view, |d| {
                                        d.child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child("Choose a folder"),
                                        )
                                    }),
                            )
                            .when(inline_controls, |d| d.child(self.toolbar(cx)))
                            .child(
                                Button::new("disk-rescan")
                                    .ghost()
                                    .small()
                                    .label(if scanning { "Cancel scan" } else { "Rescan" })
                                    .disabled(!scanning && self.session.path.is_none())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if this.session.job.is_some() {
                                            this.session.cancel();
                                            cx.notify();
                                        } else if let Some(path) = this.session.path.clone() {
                                            this.scan(path, cx);
                                        }
                                    })),
                            ),
                    ),
            )
            .when(!inline_controls, |d| {
                d.child(
                    h_flex()
                        .justify_end()
                        .px_4()
                        .py_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(self.toolbar(cx)),
                )
            })
    }
    fn breadcrumbs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut crumbs = Vec::new();
        if let Some(view) = &self.session.view {
            let mut id = Some(view.zoom);
            while let Some(i) = id {
                crumbs.push((i, clean(&view.tree.nodes[i].name.to_string_lossy())));
                id = view.tree.nodes[i].parent;
            }
            crumbs.reverse();
        }
        h_flex()
            .gap_1()
            .min_w_0()
            .children(crumbs.into_iter().map(|(id, label)| {
                widgets::crumb(
                    ("disk-crumb", id),
                    label,
                    self.session.view.as_ref().is_some_and(|v| v.zoom == id),
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.change(
                        |v| {
                            v.zoom = id;
                            v.selected = id;
                        },
                        cx,
                    )
                }))
            }))
    }
    fn map(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let hits = self.hits.clone();
        let theme = Theme::from(cx);
        let fill = theme.background;
        div()
            .id("disk-map")
            // h_flex centers its children: a percentage-height canvas needs a definite parent height.
            .h_full()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .relative()
            .bg(fill)
            .track_focus(&self.focus)
            .on_key_down(
                cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    this.key(&event.keystroke.key, window, cx)
                }),
            )
            .on_mouse_move(
                cx.listener(|this, event: &gpui_kit::MouseMoveEvent, _, cx| {
                    let hovered = {
                        let hits = this.hits.borrow();
                        let x = f32::from(event.position.x - hits.0.origin.x) as f64;
                        let y = f32::from(event.position.y - hits.0.origin.y) as f64;
                        hits.1
                            .iter()
                            .rev()
                            .find(|t| t.rect.contains(x, y))
                            .map(|t| t.id)
                    };
                    if this.hovered != hovered {
                        this.hovered = hovered;
                        cx.notify();
                    }
                }),
            )
            .on_hover(cx.listener(|this, hovered, _, cx| {
                if !hovered && this.hovered.take().is_some() {
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui_kit::MouseDownEvent, window, cx| {
                    this.focus.focus(window, cx);
                    let id = {
                        let hits = this.hits.borrow();
                        let x = f32::from(event.position.x - hits.0.origin.x) as f64;
                        let y = f32::from(event.position.y - hits.0.origin.y) as f64;
                        hits.1
                            .iter()
                            .rev()
                            .find(|t| t.rect.contains(x, y))
                            .map(|t| t.id)
                    };
                    if let Some(id) = id {
                        this.change(
                            |v| {
                                v.selected = id;
                                if event.modifiers.control || event.modifiers.platform {
                                    v.mark();
                                } else if event.click_count >= 2 {
                                    v.enter();
                                }
                            },
                            cx,
                        );
                    }
                }),
            )
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let this = entity.read(cx);
                        let Some(view) = &this.session.view else {
                            return Mosaic::default();
                        };
                        let tiles = view.tiles(
                            f32::from(bounds.size.width) as f64,
                            f32::from(bounds.size.height) as f64,
                            22.,
                            2.,
                        );
                        *hits.borrow_mut() = (bounds, tiles.clone());
                        let draft = this.filter.read(cx).value().to_lowercase();
                        let mut mosaic = Mosaic::default();
                        let parents: std::collections::HashSet<_> = tiles
                            .iter()
                            .filter_map(|t| view.tree.nodes[t.id].parent)
                            .collect();
                        for tile in tiles {
                            let node = &view.tree.nodes[tile.id];
                            let mut parent = Some(tile.id);
                            let mut covered = false;
                            let mut matches = draft.is_empty();
                            while let Some(id) = parent {
                                covered |= id != tile.id && view.marked.contains(&id);
                                matches |= view.tree.nodes[id]
                                    .name
                                    .to_string_lossy()
                                    .to_lowercase()
                                    .contains(&draft);
                                parent = view.tree.nodes[id].parent;
                            }
                            let filtered = if matches {
                                Filtered::Shown
                            } else if node.is_dir {
                                Filtered::Holds
                            } else {
                                Filtered::Out
                            };
                            let rect = disk::PixelRect::new(
                                tile.rect.x as f32 + 0.75,
                                tile.rect.y as f32 + 0.75,
                                (tile.rect.w as f32 - 1.5).max(0.),
                                (tile.rect.h as f32 - 1.5).max(0.),
                            );
                            let marked = view.marked.contains(&tile.id);
                            mosaic.tiles.push(TileDeco {
                                rect,
                                depth: tile.depth as u32,
                                category: node.category,
                                age_bucket: (view.options.metric == Metric::Age).then(|| {
                                    let days = node
                                        .modified
                                        .and_then(|t| t.elapsed().ok())
                                        .map_or(i64::MAX, |d| (d.as_secs() / 86400) as i64);
                                    palette::age_bucket(days)
                                }),
                                reclaimable: view.hatched(tile.id),
                                filtered,
                                unreadable: node.issue.is_some(),
                                marked,
                                covered,
                                hovered: this.hovered == Some(tile.id),
                                selected: view.selected == tile.id,
                            });
                            if rect.w >= 32. && rect.h >= 18. {
                                mosaic.labels.push(Label {
                                    text: clean(&node.name.to_string_lossy()),
                                    rect,
                                    header: parents
                                        .contains(&tile.id)
                                        .then(|| disk::PixelRect::new(rect.x, rect.y, rect.w, 22.)),
                                    depth: tile.depth as u32,
                                    dim: filtered == Filtered::Out,
                                    marked: marked || covered,
                                    size_text: if view.options.metric == Metric::Files {
                                        disk::human_count(view.files[tile.id])
                                    } else {
                                        disk::human_bytes_short(view.bytes[tile.id])
                                    },
                                });
                            }
                        }
                        // Bound text shaping cost while retaining the largest, parent-first labels.
                        mosaic.labels.truncate(150);
                        mosaic
                    },
                    move |bounds, mosaic, window, cx| {
                        treemap_view::paint(&mosaic, bounds, &theme, window, cx)
                    },
                )
                .size_full(),
            )
    }
    fn review(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut ids: Vec<_> = self
            .session
            .view
            .as_ref()
            .map(|v| v.marked.iter().copied().collect())
            .unwrap_or_default();
        ids.sort_unstable();
        v_flex()
            .id("disk-review")
            .flex_1()
            .min_w_0()
            .p_5()
            .gap_3()
            .overflow_y_scroll()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Review marked paths"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("A read-only review list. No deletion actions are provided."),
            )
            .when(ids.is_empty(), |d| {
                d.child("Nothing marked. Select a tile and press Space.")
            })
            .children(
                ids.into_iter()
                    .map(|id| upstream::panels::mark_row(self, id, cx)),
            )
            .child(
                Button::new("disk-review-back")
                    .outline()
                    .label("Back to treemap")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.reviewing = false;
                        cx.notify();
                    })),
            )
    }
    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        upstream::panels::sidebar(self, cx)
    }
}

impl Render for DiskTree {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = self.session.view.as_ref();
        let totals = view.map_or_else(
            || "Choose a folder to begin".into(),
            |v| {
                format!(
                    "{} · {} files · {} dirs{}{}",
                    disk::format_size(v.bytes[v.zoom]),
                    v.files[v.zoom],
                    v.dirs[v.zoom],
                    if v.tree.skipped > 0 {
                        format!(" · {} skipped", v.tree.skipped)
                    } else {
                        String::new()
                    },
                    if v.tree.truncated {
                        " · Partial scan: limit reached"
                    } else {
                        ""
                    }
                )
            },
        );
        let status = if let Some(job) = &self.session.job {
            format!(
                "Scanning {} · {} entries · {} measured",
                clean(&job.root.display().to_string()),
                job.progress.entries.load(Ordering::Relaxed),
                disk::format_size(job.progress.bytes.load(Ordering::Relaxed))
            )
        } else if let Some(error) = &self.session.error {
            clean(error)
        } else if let Some(v) = view {
            format!(
                "Read-only · {:.1}s scan · Allocated blocks may overlap on copy-on-write volumes",
                v.tree.elapsed.as_secs_f64()
            )
        } else {
            "Select a folder; file contents are not read.".into()
        };
        let narrow = f32::from(window.viewport_size().width) < 1240.;

        v_flex().size_full().bg(cx.theme().background).text_color(cx.theme().foreground)
            .child(self.header(window, cx))
            .child(h_flex().px_4().py_2().text_xs().gap_3().child(totals).when(!narrow, |d| d.child(upstream::panels::legend(self, cx))).child(div().flex_1()).child(div().w(px(220.)).child(Input::new(&self.filter).small())))
            .child(h_flex().flex_1().min_h_0().child(if self.reviewing {self.review(cx).into_any_element()}else if self.session.view.as_ref().is_some_and(|v|v.tree.nodes[v.zoom].children.iter().all(|i|v.weights[*i]<=0.)){v_flex().h_full().flex_1().justify_center().items_center().gap_3().child("No entries in this view").child("Try Files mode, include hidden files, or clear the filter.").into_any_element()}else if self.session.view.is_some(){self.map(cx).into_any_element()}else{v_flex().flex_1().p_8().gap_4().justify_center().items_center().child(div().text_2xl().child("See what fills your disk")).child(div().text_sm().child("Choose a folder to explore its files and directories as a treemap.")).child(Button::new("disk-start").primary().label("Choose folder…").on_click(cx.listener(|this,_,_,cx|this.choose(cx)))).into_any_element()}).child(self.sidebar(cx)))
            .child(div().px_4().py_2().border_t_1().border_color(cx.theme().border).text_xs().text_color(cx.theme().muted_foreground).child(status))
            .child(upstream::panels::key_hints(cx))
    }
}
