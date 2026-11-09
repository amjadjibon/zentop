use crate::{
    sampler::{Command, Sampler},
    theme::{self, Theme},
};
use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    widgets::TableState,
    DefaultTerminal,
};
use std::{collections::HashSet, io, sync::mpsc::TryRecvError, time::Duration};
use zentop_core::{
    process_view::{
        self as view, Column, Filters, Identity, Query, RuleKind, Sort, TreeRow, STATUSES,
    },
    sampling::Snapshot,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    Process(Identity),
    Resource(usize),
    Filters,
    Columns,
    Settings,
    Help,
    Alerts,
    Views,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    None,
    Quit,
    Pause(bool),
    Interval(u64),
}

pub struct App {
    pub features: zentop_core::monitoring_preferences::Store,
    pub feature_error: Option<String>,
    pub notification_error: Option<String>,
    pub alerts: zentop_core::alerts::Center,
    pub notifications: zentop_core::alerts::DesktopNotifications,
    pub view_name: Option<String>,
    pub disk: crate::disktree::DiskTree,
    pub snapshot: Option<Snapshot>,
    pub error: Option<String>,
    pub sort: Sort,
    pub table: TableState,
    pub rows: Vec<TreeRow>,
    pub matching: usize,
    pub search: String,
    pub searching: bool,
    pub filters: Filters,
    pub tree: bool,
    pub expanded: HashSet<Identity>,
    pub pinned: HashSet<String>,
    pub columns: Vec<Column>,
    pub panel: Option<Panel>,
    pub panel_index: usize,
    pub detail_scroll: u16,
    pub environment: bool,
    pub paused: bool,
    pub interval: u64,
    pub themes: Vec<Theme>,
    pub theme_index: usize,
    pub gpu_index: usize,
}

impl App {
    pub fn new(interval: u64, theme_name: &str) -> io::Result<Self> {
        let themes = theme::load()?;
        let theme_index = themes
            .iter()
            .position(|t| t.name == theme_name)
            .ok_or_else(|| io::Error::other(format!("Unknown theme: {theme_name}")))?;
        Ok(Self {
            features: Default::default(),
            feature_error: None,
            notification_error: None,
            alerts: Default::default(),
            notifications: Default::default(),
            view_name: None,
            disk: crate::disktree::DiskTree::default(),
            snapshot: None,
            error: None,
            sort: Sort::default(),
            table: TableState::default(),
            rows: Vec::new(),
            matching: 0,
            search: String::new(),
            searching: false,
            filters: Filters::default(),
            tree: false,
            expanded: HashSet::new(),
            pinned: HashSet::new(),
            columns: vec![
                Column::Name,
                Column::Pid,
                Column::Status,
                Column::User,
                Column::Cpu,
                Column::Memory,
                Column::Virtual,
                Column::Disk,
                Column::RunTime,
            ],
            panel: None,
            panel_index: 0,
            detail_scroll: 0,
            environment: false,
            paused: false,
            interval,
            themes,
            theme_index,
            gpu_index: 0,
        })
    }

    pub fn theme(&self) -> &Theme {
        &self.themes[self.theme_index]
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal, sampler: &Sampler) -> io::Result<()> {
        let mut dirty = true;
        let mut demand = (false, false);
        loop {
            if self.disk.tick() {
                dirty = true;
            }
            match sampler.readings.try_recv() {
                Ok(Ok(snapshot)) if snapshot.paused == self.paused => {
                    let selected = self.selected_identity();
                    self.error = snapshot
                        .error
                        .clone()
                        .or_else(|| snapshot.port_error.clone());
                    let settings = &self.features.config.alerts;
                    let events = self.alerts.observe(
                        &snapshot.frame,
                        !snapshot.paused && snapshot.error.is_none(),
                        Duration::from_millis(self.interval),
                        settings,
                        std::time::Instant::now(),
                    );
                    if settings.desktop_notifications {
                        for event in events {
                            if let Err(error) = self.notifications.send(event.message) {
                                self.notification_error = Some(error);
                            }
                        }
                    }
                    self.snapshot = Some(snapshot);
                    self.rebuild(selected);
                    dirty = true;
                }
                Ok(Ok(_)) => {} // Ignore a queued frame from before a pause/resume request.
                Ok(Err(error)) => {
                    self.alerts.suspend();
                    self.error = Some(error);
                    dirty = true;
                }
                Err(TryRecvError::Disconnected) => {
                    return Err(io::Error::other("Sampling worker stopped"))
                }
                Err(TryRecvError::Empty) => {}
            }
            if let Some(result) = self.notifications.poll_result() {
                self.notification_error = result.err();
                dirty = true;
            }
            if dirty {
                terminal.draw(|frame| crate::ui::draw(self, frame))?;
                dirty = false;
            }
            if event::poll(Duration::from_millis(100))? {
                match event::read()? {
                    Event::Key(key) if key.is_press() => {
                        match self.key(key) {
                            Effect::Quit => return Ok(()),
                            Effect::Pause(paused) => sampler.send(Command::Pause(paused))?,
                            Effect::Interval(interval) => {
                                sampler.send(Command::Interval(interval))?
                            }
                            Effect::None => {}
                        }
                        let requested = (
                            self.columns.contains(&Column::Gpu) || self.filters.gpu.enabled,
                            view::has_port_query(&self.search)
                                || matches!(self.panel, Some(Panel::Process(_))),
                        );
                        if requested != demand {
                            sampler.send(Command::Demand {
                                gpu: requested.0,
                                ports: requested.1,
                            })?;
                            demand = requested;
                        }
                        dirty = true;
                    }
                    Event::Resize(_, _) => dirty = true,
                    _ => {}
                }
            }
        }
    }

    pub fn selected_identity(&self) -> Option<Identity> {
        let row = self.rows.get(self.table.selected()?)?;
        let p = self.snapshot.as_ref()?.frame.processes.get(row.index)?;
        Some(view::identity(p))
    }

    pub fn rebuild(&mut self, selected: Option<Identity>) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let processes = &snapshot.frame.processes;
        let query = Query::new(&self.search);
        let matches: HashSet<_> = processes
            .iter()
            .enumerate()
            .filter(|(_, p)| view::matches(p, &query, &self.filters))
            .map(|(i, _)| i)
            .collect();
        self.matching = matches.len();
        let mut order: Vec<_> = (0..processes.len()).collect();
        self.rows = if self.tree {
            let restricted = !self.search.is_empty() || self.filters.active_count() > 0;
            view::build_tree(
                processes,
                &order,
                &self.expanded,
                restricted.then_some(&matches),
                self.sort,
                &self.pinned,
            )
            .0
        } else {
            order.retain(|i| matches.contains(i));
            view::sort(&mut order, processes, self.sort, &self.pinned);
            order
                .into_iter()
                .map(|i| TreeRow {
                    index: i,
                    root_index: 0,
                    depth: 0,
                    child_count: 0,
                    expanded: false,
                    family_cpu: processes[i].cpu_usage as f64,
                    family_memory: processes[i].memory_usage,
                    family_count: 1,
                })
                .collect()
        };
        let index = selected
            .and_then(|id| {
                self.rows
                    .iter()
                    .position(|row| view::identity(&processes[row.index]) == id)
            })
            .unwrap_or_else(|| {
                self.table
                    .selected()
                    .unwrap_or(0)
                    .min(self.rows.len().saturating_sub(1))
            });
        self.table.select((!self.rows.is_empty()).then_some(index));
    }

    pub fn key(&mut self, key: KeyEvent) -> Effect {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Effect::Quit;
        }
        if self.view_name.is_some() {
            if key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            {
                return Effect::None;
            }
            return self.name_key(key.code);
        }
        if self.disk.active {
            return self.disk.key(key);
        }
        if self.searching {
            match key.code {
                KeyCode::Enter => self.searching = false,
                KeyCode::Esc => {
                    self.searching = false;
                    self.search.clear();
                }
                KeyCode::Backspace => {
                    self.search.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.search.clear()
                }
                KeyCode::Char(c)
                    if !c.is_control()
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.search.push(c)
                }
                _ => {}
            }
            self.table.select(Some(0));
            self.rebuild(None);
            return Effect::None;
        }
        if key.code == KeyCode::Char('T') {
            self.theme_index = (self.theme_index + 1) % self.themes.len();
            return Effect::None;
        }
        if let Some(panel) = self.panel {
            return self.panel_key(panel, key.code);
        }
        let selected = self.selected_identity();
        let mut preserve = true;
        match key.code {
            KeyCode::Char('q') => return Effect::Quit,
            KeyCode::Char('d') => {
                self.disk.open(std::env::current_dir().unwrap_or_default());
            }
            KeyCode::Char('/') => self.searching = true,
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.searching = true
            }
            KeyCode::Esc | KeyCode::Char('x') => {
                self.search.clear();
                self.filters = Filters::default();
                preserve = false;
            }
            KeyCode::Char('a') => self.open(Panel::Alerts),
            KeyCode::Char('b') => self.open(Panel::Views),
            KeyCode::Char('p') => {
                self.alerts.suspend();
                self.paused = !self.paused;
                return Effect::Pause(self.paused);
            }
            KeyCode::Char('t') => {
                self.tree = !self.tree;
                preserve = false;
            }
            KeyCode::Char('f') => self.open(Panel::Filters),
            KeyCode::Char('v') => self.open(Panel::Columns),
            KeyCode::Char(',') => self.open(Panel::Settings),
            KeyCode::Char('?') => self.open(Panel::Help),
            KeyCode::Char('1'..='6') => {
                if let KeyCode::Char(c) = key.code {
                    self.open(Panel::Resource(c as usize - '1' as usize));
                }
            }
            KeyCode::Enter => {
                if let Some(id) = selected {
                    self.open(Panel::Process(id));
                }
            }
            KeyCode::Char(' ') => {
                if let Some(row) = self.table.selected().and_then(|i| self.rows.get(i)) {
                    if let Some(snapshot) = &self.snapshot {
                        let command = snapshot.frame.processes[row.index].command.clone();
                        if !self.pinned.remove(&command) {
                            self.pinned.insert(command);
                        }
                    }
                }
            }
            KeyCode::Char('c') => {
                self.sort.column = Column::Cpu;
                self.sort.ascending = false;
                preserve = false;
            }
            KeyCode::Char('m') => {
                self.sort.column = Column::Memory;
                self.sort.ascending = false;
                preserve = false;
            }
            KeyCode::Char('r') => {
                self.sort.ascending = !self.sort.ascending;
                preserve = false;
            }
            KeyCode::Char('[') | KeyCode::Char(']') => {
                let i = self
                    .columns
                    .iter()
                    .position(|c| *c == self.sort.column)
                    .unwrap_or(0);
                let next = if key.code == KeyCode::Char(']') {
                    (i + 1) % self.columns.len()
                } else {
                    (i + self.columns.len() - 1) % self.columns.len()
                };
                self.sort.column = self.columns[next];
                preserve = false;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.table.select_next();
                return Effect::None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.table.select_previous();
                return Effect::None;
            }
            KeyCode::PageDown => {
                self.table
                    .select(Some(self.table.selected().unwrap_or(0).saturating_add(10)));
                return Effect::None;
            }
            KeyCode::PageUp => {
                self.table
                    .select(Some(self.table.selected().unwrap_or(0).saturating_sub(10)));
                return Effect::None;
            }
            KeyCode::Home => {
                self.table.select(Some(0));
                return Effect::None;
            }
            KeyCode::End => {
                self.table.select(Some(self.rows.len().saturating_sub(1)));
                return Effect::None;
            }
            KeyCode::Right => {
                if let Some(id) = selected {
                    self.expanded.insert(id);
                }
            }
            KeyCode::Left => {
                if let Some(id) = selected {
                    self.expanded.remove(&id);
                }
            }
            _ => {}
        }
        if !preserve {
            self.table.select(Some(0));
        }
        self.rebuild(if preserve { selected } else { None });
        Effect::None
    }

    fn open(&mut self, panel: Panel) {
        if matches!(panel, Panel::Alerts | Panel::Views) {
            self.feature_error = self.features.reload().err();
        }
        self.panel = Some(panel);
        self.panel_index = 0;
        self.detail_scroll = 0;
        self.environment = false;
    }

    fn panel_key(&mut self, panel: Panel, code: KeyCode) -> Effect {
        if matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
            self.panel = None;
            return Effect::None;
        }
        match panel {
            Panel::Alerts | Panel::Views => return self.features_key(panel, code),
            Panel::Filters => {
                match code {
                    KeyCode::Up => self.panel_index = self.panel_index.saturating_sub(1),
                    KeyCode::Down => self.panel_index = (self.panel_index + 1).min(4),
                    KeyCode::Char(' ') | KeyCode::Enter if self.panel_index < 4 => {
                        let rule = self.filters.rule_mut(RuleKind::ALL[self.panel_index]);
                        rule.enabled = !rule.enabled;
                    }
                    KeyCode::Left | KeyCode::Right | KeyCode::Char('+') | KeyCode::Char('-') => {
                        let positive = matches!(code, KeyCode::Right | KeyCode::Char('+'));
                        if self.panel_index < 4 {
                            let rule = self.filters.rule_mut(RuleKind::ALL[self.panel_index]);
                            let step = if self.panel_index == 2 { 100. } else { 1. };
                            rule.value = (rule.value + if positive { step } else { -step }).max(0.);
                        } else {
                            let current = self
                                .filters
                                .statuses
                                .first()
                                .and_then(|s| STATUSES.iter().position(|v| v == s))
                                .map_or(0, |i| i + 1);
                            let next = (current + if positive { 1 } else { STATUSES.len() })
                                % (STATUSES.len() + 1);
                            self.filters.statuses = if next == 0 {
                                vec![]
                            } else {
                                vec![STATUSES[next - 1].into()]
                            };
                        }
                    }
                    KeyCode::Char('o') if self.panel_index < 4 => {
                        let rule = self.filters.rule_mut(RuleKind::ALL[self.panel_index]);
                        let all = view::Operator::ALL;
                        let current = all.iter().position(|o| *o == rule.operator).unwrap_or(0);
                        rule.operator = all[(current + 1) % all.len()];
                    }
                    KeyCode::Char('x') => self.filters = Filters::default(),
                    _ => {}
                }
                self.table.select(Some(0));
                self.rebuild(None);
            }
            Panel::Columns => match code {
                KeyCode::Up => self.panel_index = self.panel_index.saturating_sub(1),
                KeyCode::Down => {
                    self.panel_index = (self.panel_index + 1).min(Column::ALL.len() - 1)
                }
                KeyCode::Char(' ') | KeyCode::Enter => {
                    let col = Column::ALL[self.panel_index];
                    if col != Column::Name {
                        if self.columns.contains(&col) {
                            self.columns.retain(|c| *c != col);
                        } else {
                            self.columns.push(col);
                        }
                    }
                }
                _ => {}
            },
            Panel::Settings => {
                if matches!(code, KeyCode::Left | KeyCode::Right) {
                    let rates = [1500, 2000, 3000, 5000, 10000, 30000];
                    let current = rates.iter().position(|n| *n == self.interval).unwrap_or(2);
                    let next = (current
                        + if code == KeyCode::Right {
                            1
                        } else {
                            rates.len() - 1
                        })
                        % rates.len();
                    self.interval = rates[next];
                    return Effect::Interval(self.interval);
                }
            }
            _ => match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.detail_scroll = self.detail_scroll.saturating_sub(1)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.detail_scroll = self.detail_scroll.saturating_add(1)
                }
                KeyCode::Home => self.detail_scroll = 0,
                KeyCode::Char('e') if matches!(panel, Panel::Process(_)) => {
                    self.environment = !self.environment
                }
                KeyCode::Char('g') if matches!(panel, Panel::Resource(2)) => {
                    let count = self.snapshot.as_ref().map_or(0, |s| s.frame.gpus.len());
                    if count > 0 {
                        self.gpu_index = (self.gpu_index + 1) % count;
                    }
                }
                _ => {}
            },
        }
        Effect::None
    }
}
