//! Desktop-inspired resource cards, process toolbar and detail sheets.
use crate::{
    app::{App, Panel},
    theme::Theme,
};
use ratatui::{
    layout::{Constraint, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Cell, Clear, Gauge, Paragraph, Row, Sparkline, Table, Wrap},
    Frame,
};
use zentop_core::{
    process_view::{self as view, Column, RuleKind},
    sampling::{Sample, Snapshot},
};

pub fn plain(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).collect()
}
fn percent(v: Option<f64>) -> String {
    v.filter(|v| v.is_finite())
        .map_or_else(|| "Unavailable".into(), |v| format!("{v:.1}%"))
}
fn ratio(used: u64, total: u64) -> Option<f64> {
    (total > 0).then(|| used as f64 / total as f64 * 100.)
}
fn rate(value: Option<u64>) -> String {
    value.map_or_else(
        || "Unavailable".into(),
        |v| format!("{}/s", view::format_bytes(v)),
    )
}
fn block<'a>(title: impl Into<Line<'a>>, theme: &Theme) -> Block<'a> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .title(title)
        .title_style(Style::default().fg(theme.muted_foreground))
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.card).fg(theme.foreground))
}

pub fn draw(app: &mut App, frame: &mut Frame) {
    let theme = app.theme().clone();
    if app.disk.active {
        app.disk.draw(frame, &theme);
        return;
    }
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.background).fg(theme.foreground)),
        area,
    );
    let compact = area.width < 65 || area.height < 20;
    let card_rows = if area.width >= 175 {
        1
    } else if area.width >= 78 {
        2
    } else {
        3
    };
    let card_height = if area.height >= 36 {
        8
    } else if area.height >= 28 {
        6
    } else {
        4
    };
    let cards_height = if compact { 3 } else { card_rows * card_height };
    let [header, cards, toolbar, table, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(cards_height),
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .areas(area);
    let [brand, status] =
        Layout::horizontal([Constraint::Min(12), Constraint::Length(42)]).areas(header);
    frame.render_widget(
        Paragraph::new(" ◌ Zentop").style(
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ),
        brand,
    );
    frame.render_widget(
        Paragraph::new(format!(
            "{} · {:.1}s   {}  [T]",
            if app.paused { "Ⅱ Paused" } else { "● Live" },
            app.interval as f64 / 1000.,
            theme.label
        ))
        .style(Style::default().fg(if app.paused {
            theme.muted_foreground
        } else {
            theme.status_running
        })),
        status,
    );
    if let Some(snapshot) = &app.snapshot {
        if compact {
            compact_cards(frame, cards, snapshot, &theme);
        } else {
            let bands =
                Layout::vertical(vec![Constraint::Fill(1); card_rows as usize]).split(cards);
            let per_row = 7_u16.div_ceil(card_rows);
            for (r, band) in bands.iter().enumerate() {
                let boxes =
                    Layout::horizontal(vec![Constraint::Fill(1); per_row as usize]).split(*band);
                for (c, rect) in boxes.iter().enumerate() {
                    if r * per_row as usize + c >= 7 {
                        break;
                    }
                    card(
                        frame,
                        *rect,
                        r * per_row as usize + c,
                        snapshot,
                        app.gpu_index,
                        &theme,
                    );
                }
            }
        }
    } else {
        frame.render_widget(
            Paragraph::new("Collecting system readings…").block(block("Resources", &theme)),
            cards,
        );
    }
    let mode = if app.tree {
        "List / [Tree]"
    } else {
        "[List] / Tree"
    };
    let search = if app.search.is_empty() && !app.searching {
        "Search processes…  [/]".into()
    } else {
        format!(
            "Search: {}{}",
            plain(&app.search),
            if app.searching { "▏" } else { "" }
        )
    };
    let title = format!(
        " Processes {}  · {} [t] · Filters {} [f] · Columns [v] ",
        app.matching,
        mode,
        app.filters.active_count()
    );
    frame.render_widget(
        Paragraph::new(search).block(block(title, &theme).border_style(Style::default().fg(
            if app.searching {
                theme.primary
            } else {
                theme.border
            },
        ))),
        toolbar,
    );
    process_table(app, frame, table, &theme);
    let hint = app
        .error
        .as_ref()
        .map(|e| format!("Readings may be stale: {}", plain(e)))
        .unwrap_or_else(|| {
            if app.searching {
                "Type a name, command, PID, regex or :port · Enter apply · Esc clear".into()
            } else {
                "↑↓ scroll  Enter inspect  Space pin  p pause  1–7 resources  d DiskTree  a alerts  b views  ? help  q quit".into()
            }
        });
    let hint = app
        .feature_error
        .as_ref()
        .or(app.notification_error.as_ref())
        .map(|e| plain(e))
        .or_else(|| {
            app.alerts
                .history
                .front()
                .filter(|e| zentop_core::sampling::now_ms().saturating_sub(e.timestamp) < 60000)
                .map(|e| format!("Alert: {}  · a history", plain(&e.message)))
        })
        .unwrap_or(hint);
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(theme.muted_foreground)),
        footer,
    );
    if let Some(panel) = app.panel {
        panel_view(app, frame, panel, &theme);
    }
}

fn compact_cards(frame: &mut Frame, rect: Rect, snapshot: &Snapshot, theme: &Theme) {
    let stats = &snapshot.frame.stats;
    let cpu = if snapshot.frame.sequence < 2 {
        "Warming up".into()
    } else {
        percent(snapshot.history.last().and_then(|s| s.cpu))
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!(
                "CPU {cpu} · RAM {} / {} · GPU {}",
                view::format_bytes(stats.memory_used),
                view::format_bytes(stats.memory_total),
                percent(snapshot.frame.gpus.first().and_then(|g| g.utilization))
            )),
            Line::from(format!(
                "Storage {} free · Up {} · Network ↓ {} ↑ {}",
                view::format_bytes(stats.disk_free_bytes),
                view::format_uptime(stats.uptime),
                rate(stats.network_rx_bytes),
                rate(stats.network_tx_bytes)
            )),
        ])
        .style(Style::default().fg(theme.foreground)),
        rect,
    );
}

/// Bin by time rather than stretching missing/paused samples into real readings.
fn trend(
    snapshot: &Snapshot,
    width: u16,
    value: impl Fn(&Sample) -> Option<u64>,
) -> Vec<Option<u64>> {
    let end = snapshot
        .history
        .last()
        .map_or(snapshot.frame.captured_at, |s| s.timestamp);
    (0..width)
        .map(|i| {
            let timestamp = end
                .saturating_sub((u64::from(width - 1 - i) * 120000) / u64::from(width.max(2) - 1));
            let sample = snapshot
                .history
                .iter()
                .rev()
                .find(|s| s.timestamp <= timestamp)?;
            (timestamp.saturating_sub(sample.timestamp) <= sample.interval * 3 / 2)
                .then(|| value(sample))
                .flatten()
        })
        .collect()
}

fn card(
    frame: &mut Frame,
    rect: Rect,
    index: usize,
    snapshot: &Snapshot,
    gpu_index: usize,
    theme: &Theme,
) {
    let stats = &snapshot.frame.stats;
    let gpu = snapshot
        .frame
        .gpus
        .get(gpu_index)
        .or_else(|| snapshot.frame.gpus.first());
    let warming = snapshot.frame.sequence < 2;
    let (title, accent, headline, caption) = match index {
        0 => (
            "CPU",
            theme.chart_1,
            if warming {
                "Warming up".into()
            } else {
                percent(snapshot.history.last().and_then(|s| s.cpu))
            },
            format!("{} logical cores", stats.cpu_usage.len()),
        ),
        1 => (
            "Memory",
            theme.chart_3,
            if stats.memory_total > 0 {
                view::format_bytes(stats.memory_used)
            } else {
                "Unavailable".into()
            },
            if stats.memory_total > 0 {
                format!("used of {}", view::format_bytes(stats.memory_total))
            } else {
                "RAM not reported".into()
            },
        ),
        2 => (
            "GPU",
            theme.chart_2,
            percent(gpu.and_then(|g| g.utilization)),
            gpu.map_or_else(|| "No adapter reported".into(), |g| plain(&g.name)),
        ),
        3 => (
            "Storage",
            theme.chart_4,
            if stats.disk_total_bytes > 0 {
                format!("{} free", view::format_bytes(stats.disk_free_bytes))
            } else {
                "Unavailable".into()
            },
            if stats.disk_total_bytes > 0 {
                format!("of {}", view::format_bytes(stats.disk_total_bytes))
            } else {
                "Capacity not reported".into()
            },
        ),
        6 => (
            "Temperature",
            theme.chart_3,
            zentop_core::temperature::format(
                zentop_core::temperature::hottest(&stats.temperatures).and_then(|s| s.celsius),
            ),
            "Hottest reported sensor".into(),
        ),
        4 => (
            "System",
            theme.foreground,
            view::format_uptime(stats.uptime),
            format!(
                "Load {:.2} {:.2} {:.2}",
                stats.load_avg[0], stats.load_avg[1], stats.load_avg[2]
            ),
        ),
        _ => (
            "Network",
            theme.chart_5,
            format!("↓ {}", rate(stats.network_rx_bytes)),
            format!("↑ {}", rate(stats.network_tx_bytes)),
        ),
    };
    let border = block(
        Line::styled(
            format!(" {} {} ↗ ", index + 1, title),
            Style::default().fg(accent),
        ),
        theme,
    );
    let inner = border.inner(rect);
    frame.render_widget(border, rect);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                headline,
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::styled(caption, Style::default().fg(theme.muted_foreground)),
        ]),
        inner,
    );
    if inner.height >= 4 {
        let chart = Rect {
            x: inner.x,
            y: inner.bottom() - 2,
            width: inner.width,
            height: 1,
        };
        match index {
            0 | 1 | 2 | 5 | 6 => {
                let data = trend(snapshot, chart.width, |s| match index {
                    0 => s.cpu.map(|n| (n * 10.) as u64),
                    1 => s.memory.map(|n| (n * 10.) as u64),
                    2 => gpu
                        .and_then(|g| s.gpus.get(&g.id).copied().flatten())
                        .map(|n| (n * 10.) as u64),
                    6 => s
                        .temperatures
                        .values()
                        .filter_map(|v| *v)
                        .reduce(f64::max)
                        .map(|v| ((v + 40.) * 10.) as u64),
                    _ => s.received.zip(s.sent).map(|(r, t)| r.saturating_add(t)),
                });
                let max = if index == 5 {
                    data.iter().flatten().copied().max().unwrap_or(1).max(1)
                } else if index == 6 {
                    1900
                } else {
                    1000
                };
                frame.render_widget(
                    Sparkline::default()
                        .data(data)
                        .max(max)
                        .style(Style::default().fg(accent))
                        .absent_value_symbol(" "),
                    chart,
                );
                frame.render_widget(
                    Paragraph::new(if index == 5 {
                        "2m · received + sent"
                    } else if index == 6 {
                        "2m · −40–150°C"
                    } else {
                        "2m · 0–100%"
                    })
                    .style(Style::default().fg(theme.muted_foreground)),
                    Rect {
                        y: inner.bottom() - 1,
                        height: 1,
                        ..inner
                    },
                );
            }
            3 => {
                if let Some(value) = ratio(stats.disk_used_bytes, stats.disk_total_bytes) {
                    frame.render_widget(
                        Gauge::default()
                            .ratio((value / 100.).clamp(0., 1.))
                            .label(format!("{value:.1}% used"))
                            .gauge_style(Style::default().fg(accent).bg(theme.accent)),
                        chart,
                    );
                } else {
                    frame.render_widget(Paragraph::new("Capacity unavailable"), chart);
                }
            }
            _ => {
                frame.render_widget(
                    Paragraph::new("1 min   5 min   15 min")
                        .style(Style::default().fg(theme.muted_foreground)),
                    chart,
                );
            }
        }
    }
}

fn column_width(column: Column) -> u16 {
    match column {
        Column::Name => 23,
        Column::Pid | Column::Ppid => 8,
        Column::Cpu | Column::Gpu => 8,
        Column::Status => 10,
        Column::Memory | Column::Virtual => 12,
        Column::Disk => 18,
        Column::RunTime => 15,
        Column::StartTime => 20,
        _ => 14,
    }
}

fn fitting_columns(app: &App, width: u16) -> Vec<Column> {
    let mut budget = width.saturating_sub(4 + column_width(Column::Name));
    let mut chosen = vec![Column::Name];
    let priority = [Column::Pid, Column::Cpu, Column::Memory, Column::Status];
    for c in priority.into_iter().chain(app.columns.iter().copied()) {
        if c == Column::Name || !app.columns.contains(&c) || chosen.contains(&c) {
            continue;
        }
        let cost = column_width(c) + 1;
        if budget >= cost {
            budget -= cost;
            chosen.push(c);
        }
    }
    app.columns
        .iter()
        .filter(|c| chosen.contains(c))
        .copied()
        .collect()
}

fn process_table(app: &mut App, frame: &mut Frame, rect: Rect, theme: &Theme) {
    let Some(snapshot) = &app.snapshot else {
        frame.render_widget(
            Paragraph::new("Waiting for the first sample…").block(block("Processes", theme)),
            rect,
        );
        return;
    };
    let columns = fitting_columns(app, rect.width);
    let warming = snapshot.frame.sequence < 2;
    let rows = app.rows.iter().map(|row| {
        let p = &snapshot.frame.processes[row.index];
        Row::new(columns.iter().map(|col| {
            let text = match col {
                Column::Name => format!(
                    "{}{}{}{}",
                    if app.pinned.contains(&p.command) {
                        "◆ "
                    } else {
                        ""
                    },
                    "  ".repeat(row.depth.min(12)),
                    if row.child_count == 0 {
                        ""
                    } else if row.expanded {
                        "▾ "
                    } else {
                        "▸ "
                    },
                    plain(&p.name)
                ),
                Column::Cpu if warming => "—".into(),
                Column::Cpu if app.tree => format!("{:.1}%", row.family_cpu),
                Column::Memory if app.tree => view::format_bytes(row.family_memory),
                _ => plain(&col.text(p)),
            };
            Cell::from(text).style(Style::default().fg(
                if *col == Column::Status && p.status == "Running" {
                    theme.status_running
                } else {
                    theme.foreground
                },
            ))
        }))
    });
    let title = if app.tree {
        format!(
            " App families · combined CPU / RAM · {} matches ",
            app.matching
        )
    } else {
        format!(
            " {} / {} processes ",
            app.matching,
            snapshot.frame.processes.len()
        )
    };
    let header = Row::new(columns.iter().map(|c| {
        Cell::from(format!(
            "{}{}",
            c.label(),
            if *c == app.sort.column {
                if app.sort.ascending {
                    " ↑"
                } else {
                    " ↓"
                }
            } else {
                ""
            }
        ))
        .style(
            Style::default()
                .fg(if *c == app.sort.column {
                    theme.primary
                } else {
                    theme.muted_foreground
                })
                .add_modifier(Modifier::BOLD),
        )
    }));
    if app.rows.is_empty() {
        app.table.select(None);
    } else {
        app.table.select(Some(
            app.table.selected().unwrap_or(0).min(app.rows.len() - 1),
        ));
    }
    let table = Table::new(
        rows,
        columns.iter().map(|c| {
            if *c == Column::Name {
                Constraint::Min(12)
            } else {
                Constraint::Length(column_width(*c))
            }
        }),
    )
    .header(header)
    .block(block(title, theme))
    .column_spacing(1)
    .row_highlight_style(
        Style::default()
            .bg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("› ");
    frame.render_stateful_widget(table, rect, &mut app.table);
    if app.rows.is_empty() {
        let inner = rect.inner(Margin {
            horizontal: 2,
            vertical: 2,
        });
        frame.render_widget(
            Paragraph::new("No matching processes. Press x to clear search and filters.")
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(theme.muted_foreground)),
            inner,
        );
    }
}

fn panel_view(app: &mut App, frame: &mut Frame, panel: Panel, theme: &Theme) {
    let size = frame.area();
    let width = size.width.saturating_sub(4).clamp(1, 100).min(size.width);
    let height = size.height.saturating_sub(4).max(1).min(size.height);
    let rect = Rect::new(
        (size.width - width) / 2,
        (size.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, rect);
    let (title, lines) = panel_text(app, panel);
    let border = block(format!(" {title} · Esc to close "), theme)
        .border_style(Style::default().fg(theme.primary));
    let inner = border.inner(rect);
    frame.render_widget(border, rect);
    let text: Vec<_> = lines
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let selected = matches!(
                panel,
                Panel::Filters | Panel::Columns | Panel::Alerts | Panel::Views
            ) && i == app.panel_index + 2;
            Line::from(Span::styled(
                plain(text),
                Style::default()
                    .fg(if selected {
                        theme.primary
                    } else {
                        theme.foreground
                    })
                    .bg(if selected { theme.accent } else { theme.card }),
            ))
        })
        .collect();
    let selectable = matches!(
        panel,
        Panel::Filters | Panel::Columns | Panel::Alerts | Panel::Views
    );
    let selected_bottom = if selectable {
        Paragraph::new(
            text.iter()
                .take(app.panel_index + 3)
                .cloned()
                .collect::<Vec<_>>(),
        )
        .wrap(Wrap { trim: false })
        .line_count(inner.width)
    } else {
        0
    };
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
    let max_scroll = paragraph
        .line_count(inner.width)
        .saturating_sub(inner.height as usize)
        .min(u16::MAX as usize) as u16;
    let scroll = if selectable {
        (selected_bottom
            .saturating_sub(inner.height as usize)
            .min(u16::MAX as usize) as u16)
            .max(app.detail_scroll)
            .min(max_scroll)
    } else {
        app.detail_scroll = app.detail_scroll.min(max_scroll);
        app.detail_scroll
    };
    frame.render_widget(paragraph.scroll((scroll, 0)), inner);
}

fn panel_text(app: &App, panel: Panel) -> (String, Vec<String>) {
    match panel {
        Panel::Views => {
            let mut lines = vec![app.view_name.as_ref().map_or_else(|| "↑↓ select · Enter load · n new · u update selected · d delete".into(), |name| format!("Name: {}▏ · Enter save · Esc cancel", plain(name))), "".into()];
            if app.features.config.views.is_empty() { lines.push("No saved views. Press n to save the current view.".into()); }
            lines.extend(app.features.config.views.iter().map(|v| format!("{} · {} · {} columns · {} pins", v.name, if v.tree {"Tree"} else {"List"}, v.columns.len(), v.pinned.len())));
            lines.push("".into()); lines.push("Views include search, filters, sort, columns, List/Tree and pinned commands.".into());
            lines.push("Shared with the GUI. Saving an existing name replaces it.".into());
            if let Some(error) = &app.feature_error { lines.push(error.clone()); }
            ("Saved monitoring views".into(), lines)
        }
        Panel::Alerts => {
            use zentop_core::alerts::Kind;
            let settings = &app.features.config.alerts;
            let mut lines = vec!["↑↓ select · Space toggle · ←→ adjust · PgUp/Dn history · x clear history".into(), "".into()];
            lines.extend(Kind::ALL.into_iter().map(|kind| { let rule = settings.rule(kind); format!("[{}] {} {} {}{}", if rule.enabled {"✓"} else {" "}, kind.label(), if kind == Kind::DiskFree {"≤"} else {"≥"}, rule.threshold, if kind == Kind::Temperature {"°C"} else {"%"}) }));
            lines.push(format!("Sustained for: {}s", settings.sustain_secs));
            lines.push(format!("Repeat cooldown: {}s", settings.cooldown_secs));
            lines.push(format!("[{}] Also send desktop notifications", if settings.desktop_notifications {"✓"} else {" "}));
            lines.push("".into()); lines.push("Pause suspends alerts. Disk uses the least-free writable volume.".into());
            lines.push("Desktop delivery depends on OS notification permissions.".into());
            lines.push("".into()); lines.push("Recent alerts (this session):".into());
            if app.alerts.history.is_empty() { lines.push("No alerts yet.".into()); }
            lines.extend(app.alerts.history.iter().map(|a| format!("{}  {}", view::format_timestamp(a.timestamp/1000), a.message)));
            if let Some(error) = &app.feature_error { lines.push(error.clone()); }
            if let Some(error) = &app.notification_error { lines.push(error.clone()); }
            ("Resource alerts".into(), lines)
        }
        Panel::Help => (
            "Keyboard shortcuts".into(),
            vec![
                "/ or Ctrl+F   Search names, commands, PIDs, regex or :port",
                "Enter          Inspect selected process",
                "1–7            CPU / Memory / GPU / Storage / System / Network / Temperature",
                "↑↓ or j/k      Scroll processes or a detail sheet",
                "PageUp/Down    Move ten rows",
                "Space          Pin/unpin the selected command",
                "t              Switch List / Tree",
                "← →            Collapse / expand a process family",
                "c / m          Sort by CPU / RAM",
                "[ / ]          Choose sort column",
                "r              Reverse sort direction",
                "f              Filters: enable rules, adjust thresholds and status",
                "v              Choose table columns",
                "a              Resource alert rules and history",
                "b              Saved monitoring views",
                "p              Pause/resume sampling",
                "d              Open DiskTree for the current directory",
                ",              Settings: refresh interval",
                "T              Next theme (all 24 desktop palettes)",
                "x              Clear search and filters",
                "Esc            Close a sheet or clear search/filters",
                "q / Ctrl+C     Quit (q closes an open sheet first)",
                "",
                "Tree totals include all descendants, even hidden matches. Shared RAM may overlap.",
                "CPU can exceed 100% across cores. Missing readings remain unavailable.",
                "Save pins, columns, search and filters with b. Interval and theme last this session.",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        ),
        Panel::Settings => (
            "Settings".into(),
            vec![
                format!(
                    "Refresh interval: {:.1}s    ← → change",
                    app.interval as f64 / 1000.
                ),
                format!("Appearance: {}    T next theme", app.theme().label),
                "".into(),
                "Longer intervals reduce monitoring activity.".into(),
                "Choices apply to this terminal session; GUI preferences are unchanged.".into(),
            ],
        ),
        Panel::Filters => {
            let mut lines = vec![
                "↑↓ choose · Space toggle · ←→ threshold · o operator · x clear".into(),
                String::new(),
            ];
            for kind in RuleKind::ALL {
                let rule = app.filters.rule(kind);
                lines.push(format!(
                    "[{}] {} {} {:.0} {}",
                    if rule.enabled { "×" } else { " " },
                    kind.label(),
                    rule.operator.symbol(),
                    rule.value,
                    kind.unit()
                ));
            }
            lines.push(format!(
                "Status: {}   ← → change",
                app.filters.statuses.first().map_or("All", String::as_str)
            ));
            ("Filters".into(), lines)
        }
        Panel::Columns => {
            let mut lines = vec![
                "↑↓ choose · Space toggle · Name always stays visible".into(),
                String::new(),
            ];
            lines.extend(Column::ALL.iter().map(|c| {
                format!(
                    "[{}] {}",
                    if app.columns.contains(c) { "×" } else { " " },
                    c.label()
                )
            }));
            lines.push(
                "Narrow terminals hide columns to keep names and resource usage readable.".into(),
            );
            ("Columns".into(), lines)
        }
        Panel::Process(id) => {
            let Some(snapshot) = &app.snapshot else {
                return ("Process details".into(), vec!["No readings yet.".into()]);
            };
            let Some(p) = snapshot
                .frame
                .processes
                .iter()
                .find(|p| view::identity(p) == id)
            else {
                return (
                    "Process details".into(),
                    vec!["This process is no longer available.".into()],
                );
            };
            let all = &snapshot.frame.processes;
            let expanded = all.iter().map(view::identity).collect();
            let order: Vec<_> = (0..all.len()).collect();
            let family = view::build_tree(all, &order, &expanded, None, app.sort, &app.pinned)
                .0
                .into_iter()
                .find(|r| view::identity(&all[r.index]) == id);
            let mut lines = vec![
                format!("{} · PID {} · {}", p.name, p.pid, p.status),
                String::new(),
                format!(
                    "CPU {:.1}%       Memory {}       Virtual {}",
                    p.cpu_usage,
                    view::format_bytes(p.memory_usage),
                    view::format_bytes(p.virtual_memory)
                ),
                format!(
                    "Run time {}    Started {}",
                    Column::RunTime.text(p),
                    view::format_timestamp(p.start_time)
                ),
            ];
            if let Some(family) = family {
                lines.push(format!(
                    "Process family: {} processes · {:.1}% CPU · {} RAM",
                    family.family_count,
                    family.family_cpu,
                    view::format_bytes(family.family_memory)
                ));
            }
            lines.extend([
                format!("User: {}     Parent PID: {}", p.user, p.ppid),
                format!("Command: {}", p.command),
                format!("Root: {}", p.root),
                format!(
                    "Disk I/O: {} read / {} written",
                    view::format_bytes(p.disk_usage.0),
                    view::format_bytes(p.disk_usage.1)
                ),
                String::new(),
                format!(
                    "Local ports: {}",
                    if p.ports.is_empty() {
                        "None reported (permissions may hide sockets)".into()
                    } else {
                        p.ports
                            .iter()
                            .map(u16::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                ),
                "Ports include TCP listeners and bound UDP sockets; unavailable while paused."
                    .into(),
                String::new(),
                format!(
                    "Environment variables ({}) · e to {}",
                    p.environ.len(),
                    if app.environment { "hide" } else { "reveal" }
                ),
            ]);
            if app.environment {
                lines.extend(p.environ.iter().cloned());
            }
            ("Process details".into(), lines)
        }
        Panel::Resource(index) => resource_text(app, index),
    }
}

fn resource_text(app: &App, index: usize) -> (String, Vec<String>) {
    let title = [
        "CPU",
        "Memory",
        "Graphics",
        "Storage",
        "System",
        "Network",
        "Temperature",
    ][index]
        .to_string();
    let Some(snapshot) = &app.snapshot else {
        return (title, vec!["No readings yet.".into()]);
    };
    let s = &snapshot.frame.stats;
    let d = &s.details;
    let lines = match index {
        6 => s
            .temperatures
            .iter()
            .map(|sensor| {
                format!(
                    "{} · {} · {} · session min {} / max {} · critical {} · {}",
                    sensor.kind,
                    sensor.name,
                    zentop_core::temperature::format(sensor.celsius),
                    zentop_core::temperature::format(sensor.minimum),
                    zentop_core::temperature::format(sensor.maximum),
                    zentop_core::temperature::format(sensor.critical),
                    sensor.source
                )
            })
            .collect(),
        0 => {
            let mut lines = vec![
                d.identity
                    .cpu_model
                    .clone()
                    .unwrap_or_else(|| "Processor unavailable".into()),
                format!(
                    "{} logical cores · {} physical cores",
                    s.cpu_usage.len(),
                    d.identity
                        .physical_cores
                        .map_or_else(|| "Unknown".into(), |v| v.to_string())
                ),
                String::new(),
            ];
            lines.extend(s.cpu_usage.iter().enumerate().map(|(i, v)| {
                if snapshot.frame.sequence < 2 {
                    format!("Core {i:>2}    Warming up")
                } else {
                    format!("Core {i:>2}    {v:.1}%")
                }
            }));
            lines
        }
        1 => vec![
            format!(
                "Used: {} / {}",
                view::format_bytes(s.memory_used),
                view::format_bytes(s.memory_total)
            ),
            format!(
                "Utilization: {}",
                percent(ratio(s.memory_used, s.memory_total))
            ),
            format!("Free pages: {}", view::format_bytes(s.memory_free)),
            format!("Cached: {}", view::format_bytes(s.memory_cached)),
            format!(
                "Swap: {} / {}",
                view::format_bytes(d.swap_used),
                view::format_bytes(d.swap_total)
            ),
            "Not in use differs from strictly free pages.".into(),
        ],
        2 => {
            if let Some(gpu) = snapshot
                .frame
                .gpus
                .get(app.gpu_index)
                .or_else(|| snapshot.frame.gpus.first())
            {
                let number = |v: Option<f64>, unit: &str| {
                    v.map_or_else(|| "Unavailable".into(), |v| format!("{v:.1}{unit}"))
                };
                vec![
                    format!("{} · {}    g next adapter", gpu.name, gpu.vendor),
                    format!("Utilization: {}", percent(gpu.utilization)),
                    format!(
                        "Memory: {} / {} ({})",
                        gpu.memory_used
                            .map_or_else(|| "Unavailable".into(), view::format_bytes),
                        gpu.memory_total
                            .map_or_else(|| "Unavailable".into(), view::format_bytes),
                        gpu.memory_kind
                    ),
                    format!("Temperature: {}", number(gpu.temperature, " °C")),
                    format!("Power: {}", number(gpu.power_watts, " W")),
                    format!(
                        "Clock: {}",
                        number(gpu.core_clock_mhz.map(|v| v as f64), " MHz")
                    ),
                    format!("Source: {}", gpu.source),
                    "Shared GPU memory is already part of system RAM.".into(),
                ]
            } else {
                vec!["No adapter reported by the operating system.".into()]
            }
        }
        3 => {
            let mut lines = vec![
                format!(
                    "Free: {} / {}",
                    view::format_bytes(s.disk_free_bytes),
                    view::format_bytes(s.disk_total_bytes)
                ),
                String::new(),
            ];
            for v in &d.volumes {
                lines.extend([
                    format!("{} · {} · {}", v.name, v.mount, v.file_system),
                    format!(
                        "  {} free / {}{}",
                        view::format_bytes(v.available),
                        view::format_bytes(v.total),
                        if v.read_only { " · Read only" } else { "" }
                    ),
                ]);
            }
            lines
        }
        4 => vec![
            format!(
                "{} {}",
                d.identity.platform,
                d.identity.os_version.as_deref().unwrap_or("")
            ),
            format!(
                "Kernel: {}",
                d.identity
                    .kernel_version
                    .as_deref()
                    .unwrap_or("Unavailable")
            ),
            format!("Architecture: {}", d.identity.architecture),
            format!("Uptime: {}", view::format_uptime(s.uptime)),
            format!(
                "Load average: {:.2} / {:.2} / {:.2} (1 / 5 / 15 min)",
                s.load_avg[0], s.load_avg[1], s.load_avg[2]
            ),
            "Load average is not CPU percentage.".into(),
        ],
        _ => {
            let mut lines = vec![
                format!(
                    "Received: {}      Sent: {}",
                    rate(s.network_rx_bytes),
                    rate(s.network_tx_bytes)
                ),
                String::new(),
            ];
            for n in &d.interfaces {
                lines.extend([
                    n.name.clone(),
                    format!(
                        "  ↓ {} · total {}",
                        rate(n.received),
                        view::format_bytes(n.received_total)
                    ),
                    format!(
                        "  ↑ {} · total {}",
                        rate(n.sent),
                        view::format_bytes(n.sent_total)
                    ),
                ]);
            }
            lines
        }
    };
    (title, lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trends_preserve_missing_samples_and_real_zero_readings() {
        let app = crate::tests_ui::fixture();
        let mut snapshot = app.snapshot.unwrap();
        snapshot.history.last_mut().unwrap().cpu = Some(0.);
        let values = trend(&snapshot, 41, |s| s.cpu.map(|v| (v * 10.) as u64));
        assert_eq!(values[20], None);
        assert_eq!(values.last(), Some(&Some(0)));
    }
}
