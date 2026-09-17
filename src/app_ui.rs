//! Full terminal workspace. Existing commands remain the execution boundary.
use crate::{
    analytics::Period,
    db::Db,
    profiles,
    tui::{self, Frame, Input, TerminalGuard, Tone},
    ui_commands::{self, Action, Form, Invocation},
    ui_data::{self, PAGES, Page, View},
    ui_job::Job,
    widget::clean,
};
use anyhow::{Context, Result, ensure};
use chrono_tz::Tz;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    terminal,
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use unicode_width::UnicodeWidthChar;

pub struct Options {
    pub no_policy: bool,
    /// Canonical working directory approved at application startup.
    pub cwd: PathBuf,
    pub db: PathBuf,
    pub config: PathBuf,
    pub sessions: PathBuf,
    pub timezone: Tz,
    pub page: Page,
}

struct Editor {
    form: Form,
    inputs: Vec<Input>,
    selected: usize,
    error: String,
}
impl Editor {
    fn new(form: Form) -> Self {
        let inputs = form
            .fields
            .iter()
            .map(|f| Input::new(f.value.clone()))
            .collect();
        Self {
            form,
            inputs,
            selected: 0,
            error: String::new(),
        }
    }
    fn build(&mut self) -> Result<Invocation> {
        for (field, input) in self.form.fields.iter_mut().zip(&self.inputs) {
            field.value.clone_from(&input.value);
        }
        ui_commands::build(&self.form)
    }
}

struct Document {
    title: String,
    lines: Vec<String>,
    scroll: usize,
}
struct Output {
    title: String,
    lines: Vec<String>,
    scroll: usize,
    follow: bool,
    status: String,
    action: Action,
    feedback_id: Option<String>,
    emitted_id: Option<String>,
    prior_execution_ids: Vec<String>,
}
enum Mode {
    Browse,
    Detail(Document),
    Form(Editor),
    Output(Output),
}
struct App {
    options: Options,
    cwd: PathBuf,
    executable: PathBuf,
    page_index: usize,
    menu_focus: bool,
    period: Period,
    view: View,
    selected: usize,
    scroll: usize,
    notice: String,
    mode: Mode,
    job: Option<Job>,
    interrupted: Arc<AtomicBool>,
}
impl App {
    fn page(&self) -> Page {
        PAGES[self.page_index]
    }
    fn reload(&mut self, db: &Db) {
        let id = self.view.items.get(self.selected).map(|i| i.id.clone());
        match ui_data::load(
            self.page(),
            db,
            &self.options.config,
            &self.cwd,
            self.options.timezone,
            self.period,
        ) {
            Ok(view) => {
                self.selected = id
                    .and_then(|id| view.items.iter().position(|i| i.id == id))
                    .unwrap_or(self.selected.min(view.items.len().saturating_sub(1)));
                self.view = view;
            }
            Err(error) => self.notice = format!("Não foi possível atualizar: {error:#}"),
        }
    }
    fn change_page(&mut self, db: &Db, index: usize) {
        self.page_index = index;
        self.selected = 0;
        self.scroll = 0;
        self.notice.clear();
        self.reload(db);
    }
    fn edit(&mut self, action: Action, selected: Option<String>) {
        let mut form = ui_commands::form(action, selected.as_deref());
        if matches!(action, Action::Report | Action::Widget) {
            form.fields[0].value = match self.period {
                Period::Hour => "hour",
                Period::Day => "day",
                Period::Month => "month",
            }
            .into();
        }
        self.mode = Mode::Form(Editor::new(form));
    }
    fn edit_feedback(&mut self, db: &Db, id: String) {
        let mut form = ui_commands::form(Action::Feedback, Some(&id));
        if let Ok(job) = crate::workflow::resolve_execution(db, &id)
            && let Some(feedback) = job.feedback
        {
            form.fields[1].value = feedback.delivered.to_string();
            form.fields[2].value = feedback.speed.to_string();
            form.fields[3].value = feedback.note;
        }
        self.mode = Mode::Form(Editor::new(form));
    }
    fn selected_id(&self) -> Option<String> {
        self.view.items.get(self.selected).map(|i| i.id.clone())
    }
    fn open_selected(&mut self) {
        if let Some(item) = self.view.items.get(self.selected) {
            self.mode = Mode::Detail(Document {
                title: item.label.clone(),
                lines: item.details.clone(),
                scroll: 0,
            });
        }
    }
    fn arguments(&self, invocation: &Invocation) -> Vec<OsString> {
        let mut args = vec![
            path_option("allow-workspace", &self.cwd),
            path_option("db", &self.options.db),
            path_option("config", &self.options.config),
            path_option("sessions", &self.options.sessions),
            format!("--timezone={}", self.options.timezone).into(),
        ];
        if self.options.no_policy {
            args.push("--no-policy".into());
        }
        args.extend(invocation.args.iter().cloned());
        args
    }
    fn launch(
        &mut self,
        invocation: Invocation,
        action: Action,
        db: &Db,
        guard: &mut Option<TerminalGuard>,
    ) -> Result<()> {
        let args = self.arguments(&invocation);
        if invocation.interactive {
            drop(guard.take());
            let status = interactive(&self.executable, &args, &self.cwd, &self.interrupted);
            *guard = Some(TerminalGuard::enter()?);
            self.mode = Mode::Browse;
            self.notice = match status {
                Ok(s) if s.success() => "Tela encerrada. Dados atualizados.".into(),
                Ok(s) => format!("Comando encerrado ({s})."),
                Err(e) => format!("Falha ao abrir comando: {e}"),
            };
            self.reload(db);
            return Ok(());
        }
        let prior_execution_ids = db.executions()?.into_iter().map(|e| e.id).collect();
        self.job = Some(Job::start(
            &self.executable,
            &args,
            &self.cwd,
            invocation.export_path,
        )?);
        self.mode = Mode::Output(Output {
            title: invocation.title,
            lines: vec![],
            scroll: 0,
            follow: true,
            status: "Em execução…".into(),
            action,
            feedback_id: None,
            emitted_id: None,
            prior_execution_ids,
        });
        Ok(())
    }
    fn submit(&mut self, db: &Db, guard: &mut Option<TerminalGuard>) -> Result<()> {
        let Mode::Form(editor) = &mut self.mode else {
            return Ok(());
        };
        let action = editor.form.action;
        match editor.build() {
            Ok(invocation) => {
                if let Err(error) = self.launch(invocation, action, db, guard) {
                    if let Mode::Form(editor) = &mut self.mode {
                        editor.error = format!("{error:#}");
                    } else {
                        self.notice = format!("{error:#}");
                    }
                }
            }
            Err(error) => editor.error = format!("{error:#}"),
        }
        Ok(())
    }
    fn poll_job(&mut self, db: &Db) {
        let Some(job) = &mut self.job else {
            return;
        };
        if let Mode::Output(out) = &mut self.mode {
            out.lines = job.lines();
            if out.action == Action::Run && out.emitted_id.is_none() {
                out.emitted_id = emitted_execution_id(&out.lines);
            }
            out.status = format!(
                "{}  ·  {}s",
                if job.cancelling() {
                    "Cancelando e salvando registro…"
                } else {
                    "Em execução…"
                },
                job.elapsed().as_secs()
            );
        }
        let result = job.poll();
        let finished = !matches!(result, Ok(None));
        if finished {
            if let Mode::Output(out) = &mut self.mode {
                out.lines = job.lines();
                if out.emitted_id.is_none() {
                    out.emitted_id = emitted_execution_id(&out.lines);
                }
                out.status = match result {
                    Ok(Some(result)) => {
                        let prefix = if result.cancelled {
                            "Cancelado"
                        } else if result.success {
                            "Concluído"
                        } else {
                            "Falhou"
                        };
                        format!("{prefix}  ·  {}", result.message)
                    }
                    Err(error) => format!("Erro: {error:#}"),
                    _ => unreachable!(),
                };
                if out.action == Action::Run {
                    out.feedback_id = db.executions().ok().and_then(|jobs| {
                        jobs.into_iter()
                            .find(|e| {
                                out.emitted_id.as_ref() == Some(&e.id)
                                    && !out.prior_execution_ids.contains(&e.id)
                                    && e.ended_at.is_some()
                            })
                            .map(|e| e.id)
                    });
                }
            }
            self.job = None;
            self.reload(db);
        }
    }
    fn key(&mut self, key: KeyEvent, db: &Db, guard: &mut Option<TerminalGuard>) -> Result<bool> {
        if key.kind == KeyEventKind::Release {
            return Ok(false);
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control && key.code == KeyCode::Char('c') {
            if let Some(job) = &mut self.job {
                job.cancel();
                return Ok(false);
            }
            if !matches!(self.mode, Mode::Browse) {
                self.mode = Mode::Browse;
                return Ok(false);
            }
            return Ok(true);
        }
        match &mut self.mode {
            Mode::Form(editor) => match key.code {
                KeyCode::Esc => self.mode = Mode::Browse,
                KeyCode::F(5) => self.submit(db, guard)?,
                KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                    if let Some(field) = editor.form.fields.get(editor.selected)
                        && field.multiline
                        && let Err(error) =
                            editor.inputs[editor.selected].insert_checked("\n", true)
                    {
                        editor.error = error;
                    }
                }
                KeyCode::Enter if editor.selected == editor.inputs.len() => {
                    self.submit(db, guard)?
                }
                KeyCode::Tab | KeyCode::Down | KeyCode::Enter => {
                    editor.selected = (editor.selected + 1).min(editor.inputs.len());
                }
                KeyCode::BackTab | KeyCode::Up => {
                    editor.selected = editor.selected.saturating_sub(1)
                }
                _ => {
                    if let Some(input) = editor.inputs.get_mut(editor.selected) {
                        if let KeyCode::Char(c) = key.code
                            && !key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        {
                            editor.error = input
                                .insert_checked(
                                    &c.to_string(),
                                    editor.form.fields[editor.selected].multiline,
                                )
                                .err()
                                .unwrap_or_default();
                        } else {
                            input.key(key);
                            editor.error.clear();
                        }
                    }
                }
            },
            Mode::Detail(doc) => match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left => self.mode = Mode::Browse,
                KeyCode::Down | KeyCode::Char('j') => doc.scroll = doc.scroll.saturating_add(1),
                KeyCode::Up | KeyCode::Char('k') => doc.scroll = doc.scroll.saturating_sub(1),
                KeyCode::PageDown => doc.scroll = doc.scroll.saturating_add(10),
                KeyCode::PageUp => doc.scroll = doc.scroll.saturating_sub(10),
                KeyCode::Home => doc.scroll = 0,
                KeyCode::End => doc.scroll = usize::MAX,
                _ => {}
            },
            Mode::Output(out) => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    if let Some(job) = &mut self.job {
                        job.cancel();
                    } else {
                        self.mode = Mode::Browse;
                    }
                }
                KeyCode::Char('f') if self.job.is_none() => {
                    if let Some(id) = out.feedback_id.clone() {
                        self.edit_feedback(db, id);
                    }
                }
                KeyCode::Up | KeyCode::PageUp => {
                    out.follow = false;
                    out.scroll = out.scroll.saturating_sub(5);
                }
                KeyCode::Down | KeyCode::PageDown => {
                    out.follow = false;
                    out.scroll = out.scroll.saturating_add(5);
                }
                KeyCode::Home => {
                    out.follow = false;
                    out.scroll = 0;
                }
                KeyCode::End => out.follow = true,
                _ => {}
            },
            Mode::Browse => match key.code {
                KeyCode::Char('q') => return Ok(true),
                KeyCode::Esc => {
                    if self.menu_focus {
                        return Ok(true);
                    }
                    self.menu_focus = true;
                }
                KeyCode::Tab | KeyCode::BackTab => self.menu_focus = !self.menu_focus,
                KeyCode::Left => self.menu_focus = true,
                KeyCode::Right => self.menu_focus = false,
                KeyCode::Up | KeyCode::Char('k') if self.menu_focus => {
                    self.change_page(db, (self.page_index + PAGES.len() - 1) % PAGES.len())
                }
                KeyCode::Down | KeyCode::Char('j') if self.menu_focus => {
                    self.change_page(db, (self.page_index + 1) % PAGES.len())
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.selected = self.selected.saturating_sub(1);
                    self.scroll = self.scroll.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.selected =
                        (self.selected + 1).min(self.view.items.len().saturating_sub(1));
                    self.scroll = self.scroll.saturating_add(1);
                }
                KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10),
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
                KeyCode::Char('r') => {
                    self.notice.clear();
                    self.reload(db);
                }
                KeyCode::Char('h' | 'd' | 'm') => {
                    self.period = match key.code {
                        KeyCode::Char('h') => Period::Hour,
                        KeyCode::Char('m') => Period::Month,
                        _ => Period::Day,
                    };
                    self.reload(db);
                }
                KeyCode::Enter => {
                    self.menu_focus = false;
                    if let Some(action) = primary(self.page()) {
                        let form = ui_commands::form(action, None);
                        if form.fields.is_empty() {
                            let invocation = ui_commands::build(&form)?;
                            if let Err(error) = self.launch(invocation, action, db, guard) {
                                self.notice = format!("{error:#}");
                            }
                        } else {
                            self.edit(action, None);
                        }
                    } else {
                        self.open_selected();
                    }
                }
                KeyCode::Char('a') if self.page() == Page::Profiles => {
                    self.edit(Action::AddProfile, None)
                }
                KeyCode::Char('c') if self.page() == Page::Profiles => {
                    let path = self.selected_id().map(PathBuf::from);
                    let image = path.map(|p| {
                        if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("md")) {
                            profiles::read(&p)
                                .ok()
                                .map(|(profile, _)| {
                                    let image = PathBuf::from(profile.source_image);
                                    if image.is_absolute() {
                                        image
                                    } else {
                                        p.parent().unwrap_or(Path::new(".")).join(image)
                                    }
                                })
                                .unwrap_or(p)
                        } else {
                            p
                        }
                    });
                    self.edit(
                        Action::CompileProfile,
                        image.map(|p| p.to_string_lossy().into_owned()),
                    );
                }
                KeyCode::Char('e') if self.page() == Page::Profiles => {
                    let profile = self.selected_id().and_then(|p| {
                        Path::new(&p)
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                    });
                    self.edit(Action::Run, profile);
                }
                KeyCode::Char('f') if self.page() == Page::Executions => {
                    if let Some(id) = self.selected_id() {
                        self.edit_feedback(db, id);
                    }
                }
                KeyCode::Char('t') if self.page() == Page::Runs => {
                    if let Some(id) = self.selected_id() {
                        self.edit(Action::Tag, Some(id));
                    }
                }
                KeyCode::Char('e') if self.page() == Page::Report => {
                    self.edit(Action::Report, None)
                }
                KeyCode::Char('f') if self.page() == Page::Trend => self.edit(Action::Trend, None),
                KeyCode::Char('n') if self.page() == Page::Price => self.edit(Action::Price, None),
                _ => {}
            },
        }
        Ok(false)
    }
    fn paste(&mut self, value: &str) {
        if let Mode::Form(editor) = &mut self.mode
            && let Some(input) = editor.inputs.get_mut(editor.selected)
        {
            editor.error = input
                .insert_checked(value, editor.form.fields[editor.selected].multiline)
                .err()
                .unwrap_or_default();
        }
    }
    fn frame(&mut self, columns: u16, rows: u16) -> Frame {
        let width = usize::from(columns).min(120);
        let height = usize::from(rows).min(38);
        let mut frame = Frame::new(width, height);
        if width < 76 || height < 24 {
            frame.content(2, "STACKPULSE", Tone::Accent);
            frame.content(4, "Amplie o terminal para 76 × 24.", Tone::Warning);
            frame.content(
                6,
                "Estado preservado · Ctrl+C para voltar/sair",
                Tone::Muted,
            );
            return frame;
        }
        frame.content(2, "STACKPULSE", Tone::Accent);
        frame.put(width - 25, 2, "● WORKSPACE LOCAL", Tone::Muted);
        frame.content(3, &tui::display_path(&self.cwd), Tone::Muted);
        match &mut self.mode {
            Mode::Browse => {
                frame.put(3, 5, "NAVEGAR", Tone::Muted);
                for (i, page) in PAGES.iter().enumerate() {
                    let chosen = i == self.page_index;
                    let marker = if chosen { "›" } else { " " };
                    frame.put(
                        3,
                        7 + i,
                        &clean(&format!("{marker} {:<17}", page.label()), 20),
                        if chosen && self.menu_focus {
                            Tone::Selected
                        } else if chosen {
                            Tone::Accent
                        } else {
                            Tone::Muted
                        },
                    );
                }
                for y in 5..height - 4 {
                    frame.put(24, y, "│", Tone::Muted);
                }
                let x = 27;
                let available = width - x - 3;
                frame.put(x, 5, &clean(&self.view.title, available), Tone::Accent);
                frame.put(x, 6, &clean(&self.view.subtitle, available), Tone::Muted);
                let top = 8;
                let bottom = height - 5;
                if self.view.items.is_empty() {
                    let mut lines = self.view.lines.clone();
                    if !self.view.empty.is_empty() {
                        lines.push(String::new());
                        lines.push(self.view.empty.clone());
                    }
                    let lines = wrapped(&lines, available);
                    self.scroll = self.scroll.min(lines.len().saturating_sub(bottom - top));
                    for (y, line) in (top..bottom).zip(lines.iter().skip(self.scroll)) {
                        frame.put(x, y, line, Tone::Text);
                    }
                } else {
                    let headers = wrapped(&self.view.lines, available);
                    let count = headers.len().min(4);
                    for (y, line) in (top..).zip(headers.iter().take(count)) {
                        frame.put(x, y, line, Tone::Text);
                    }
                    let item_top = top + if count == 0 { 0 } else { count + 1 };
                    let capacity = ((bottom.saturating_sub(item_top)) / 2).max(1);
                    let first = self.selected.saturating_sub(capacity - 1);
                    for (offset, item) in self
                        .view
                        .items
                        .iter()
                        .skip(first)
                        .take(capacity)
                        .enumerate()
                    {
                        let selected = offset + first == self.selected;
                        let marker = if selected { "›" } else { " " };
                        frame.put(
                            x,
                            item_top + offset * 2,
                            &clean(&format!("{marker} {}", item.label), available),
                            if selected && !self.menu_focus {
                                Tone::Selected
                            } else if selected {
                                Tone::Accent
                            } else {
                                Tone::Text
                            },
                        );
                        frame.put(
                            x + 2,
                            item_top + offset * 2 + 1,
                            &clean(&item.summary, available - 2),
                            Tone::Muted,
                        );
                    }
                    frame.put(
                        x,
                        height - 5,
                        &format!(
                            "{} de {} · Enter abre detalhes",
                            self.selected + 1,
                            self.view.items.len()
                        ),
                        Tone::Muted,
                    );
                }
                frame.content(
                    height - 3,
                    if self.notice.is_empty() {
                        hints(PAGES[self.page_index])
                    } else {
                        &self.notice
                    },
                    if self.notice.is_empty() {
                        Tone::Accent
                    } else {
                        Tone::Warning
                    },
                );
                frame.content(
                    height - 2,
                    "↑↓ navegar  Tab menu/conteúdo  r atualizar  h/d/m período  q sair",
                    Tone::Muted,
                );
            }
            Mode::Detail(doc) => {
                frame.content(5, &doc.title, Tone::Accent);
                let lines = wrapped(&doc.lines, width - 6);
                doc.scroll = doc.scroll.min(lines.len().saturating_sub(height - 10));
                for (y, line) in (7..height - 3).zip(lines.iter().skip(doc.scroll)) {
                    frame.content(y, line, Tone::Text);
                }
                frame.content(
                    height - 2,
                    "↑↓ / PgUp/PgDn rolar  Home/End extremos  Esc voltar",
                    Tone::Muted,
                );
            }
            Mode::Form(editor) => {
                frame.content(5, &editor.form.title, Tone::Accent);
                frame.content(6, &editor.form.description, Tone::Muted);
                let capacity = (height - 13) / 3;
                let first = editor.selected.saturating_sub(capacity.saturating_sub(1));
                for (offset, field) in editor
                    .form
                    .fields
                    .iter()
                    .enumerate()
                    .skip(first)
                    .take(capacity)
                {
                    let y = 8 + (offset - first) * 3;
                    let selected = offset == editor.selected;
                    frame.content(
                        y,
                        &format!("{}{}", field.label, if field.required { " *" } else { "" }),
                        if selected { Tone::Accent } else { Tone::Muted },
                    );
                    let (value, caret) = editor.inputs[offset].visible(width - 10);
                    frame.content(
                        y + 1,
                        &format!(
                            "{} {}",
                            if selected { "›" } else { " " },
                            if value.is_empty() { "…" } else { &value }
                        ),
                        if selected { Tone::Selected } else { Tone::Text },
                    );
                    if selected {
                        frame.caret = Some((5 + caret, y + 1));
                    }
                }
                let submit_y = height - 5;
                frame.content(
                    submit_y,
                    "[ Executar / salvar ]",
                    if editor.selected == editor.inputs.len() {
                        Tone::Selected
                    } else {
                        Tone::Accent
                    },
                );
                let hint = editor
                    .form
                    .fields
                    .get(editor.selected)
                    .map(|f| f.hint.as_str())
                    .unwrap_or("Enter executa o formulário. Esc volta sem salvar.");
                frame.content(
                    height - 4,
                    if editor.error.is_empty() {
                        hint
                    } else {
                        &editor.error
                    },
                    if editor.error.is_empty() {
                        Tone::Muted
                    } else {
                        Tone::Warning
                    },
                );
                frame.content(
                    height - 3,
                    &format!(
                        "Campo {} de {} · F5 executar · Alt+Enter nova linha no pedido",
                        (editor.selected + 1).min(editor.inputs.len()),
                        editor.inputs.len()
                    ),
                    Tone::Muted,
                );
                frame.content(
                    height - 2,
                    "Tab/Enter próximo  Shift+Tab anterior  Ctrl+U limpar  Esc voltar",
                    Tone::Muted,
                );
            }
            Mode::Output(out) => {
                frame.content(5, &out.title, Tone::Accent);
                frame.content(
                    6,
                    &out.status,
                    if self.job.is_some() {
                        Tone::Accent
                    } else {
                        Tone::Text
                    },
                );
                let lines = wrapped(&out.lines, width - 6);
                let capacity = height - 11;
                let max_scroll = lines.len().saturating_sub(capacity);
                if out.follow {
                    out.scroll = max_scroll;
                } else {
                    out.scroll = out.scroll.min(max_scroll);
                }
                if lines.is_empty() {
                    frame.content(9, "Aguardando saída do comando…", Tone::Muted);
                }
                for (y, line) in (8..height - 3).zip(lines.iter().skip(out.scroll).take(capacity)) {
                    frame.content(y, line, Tone::Text);
                }
                frame.content(
                    height - 3,
                    if self.job.is_some() {
                        "Esc / Ctrl+C cancelar · o registro será preservado"
                    } else if out.feedback_id.is_some() {
                        "f avaliar entrega e rapidez · Esc voltar"
                    } else {
                        "Esc voltar ao painel"
                    },
                    Tone::Accent,
                );
                frame.content(
                    height - 2,
                    "↑↓ / PgUp/PgDn rolar  Home início  End acompanhar saída",
                    Tone::Muted,
                );
            }
        }
        frame
    }
}

fn primary(page: Page) -> Option<Action> {
    match page {
        Page::Run => Some(Action::Run),
        Page::Import => Some(Action::Import),
        Page::Sync => Some(Action::Sync),
        Page::Widget => Some(Action::Widget),
        Page::Setup => Some(Action::Setup),
        _ => None,
    }
}

pub(crate) fn path_option(name: &str, value: &Path) -> OsString {
    let mut argument = OsString::from(format!("--{name}="));
    argument.push(value);
    argument
}

pub(crate) fn emitted_execution_id(lines: &[String]) -> Option<String> {
    lines.iter().find_map(|line| {
        let (_, id) = line
            .strip_prefix("stderr: AI Timeline · ")?
            .rsplit_once(" · registro ")?;
        uuid::Uuid::parse_str(id).ok().map(|_| id.to_owned())
    })
}

// A nested CLI can exit while raw mode is active. Restore the cooked terminal
// snapshot before entering our own guard again, including on errors/signals.
#[cfg(unix)]
struct TerminalSnapshot(libc::termios);
#[cfg(unix)]
impl TerminalSnapshot {
    fn capture() -> Result<Self> {
        let mut state = std::mem::MaybeUninit::uninit();
        let result = unsafe { libc::tcgetattr(libc::STDIN_FILENO, state.as_mut_ptr()) };
        ensure!(
            result == 0,
            "Não foi possível preservar o estado do terminal"
        );
        Ok(Self(unsafe { state.assume_init() }))
    }
}
#[cfg(unix)]
impl Drop for TerminalSnapshot {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.0);
        }
    }
}

pub(crate) fn interactive(
    executable: &Path,
    args: &[OsString],
    cwd: &Path,
    interrupted: &AtomicBool,
) -> Result<std::process::ExitStatus> {
    #[cfg(unix)]
    let _terminal = TerminalSnapshot::capture()?;
    let mut child = Command::new(executable)
        .args(args)
        .current_dir(cwd)
        .spawn()?;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if interrupted.load(Ordering::Relaxed) {
            let _ = child.kill();
            return child.wait().map_err(Into::into);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
fn hints(page: Page) -> &'static str {
    match page {
        Page::Profiles => "a adicionar imagem  c compilar  e executar perfil  Enter detalhes",
        Page::Executions => "Enter detalhes da execução  f registrar / editar feedback",
        Page::Runs => "Enter stack e métricas  t anotar benchmark / baseline",
        Page::Report => "h hora  d dia  m mês  e filtrar / exportar relatório JSON",
        Page::Trend => "Enter detalhes da tendência  f filtrar dias / perfil / benchmark",
        Page::Price => "n cadastrar tarifa por milhão de tokens  Enter detalhes",
        Page::Run => "Enter escrever pedido e escolher perfil",
        Page::Import => "Enter escolher arquivo JSON para importar",
        Page::Sync => "Enter sincronizar sessões locais do Codex",
        Page::Widget => "Enter abrir widget compacto · q retorna ao painel",
        Page::Setup => "Enter abrir setup dos CLIs e perfis",
        _ => "Enter abrir detalhes · h hora  d dia  m mês",
    }
}

fn wrapped(lines: &[String], width: usize) -> Vec<String> {
    let mut output = Vec::new();
    if width == 0 {
        return output;
    }
    for text in lines {
        for line in text.split('\n') {
            let mut chunk = String::new();
            let mut size = 0;
            for c in line.chars().filter(|c| !c.is_control()) {
                let w = c.width().unwrap_or(0);
                if size + w > width && !chunk.is_empty() {
                    output.push(std::mem::take(&mut chunk));
                    size = 0;
                }
                if w <= width {
                    chunk.push(c);
                    size += w;
                }
            }
            output.push(chunk);
        }
    }
    output
}

struct Signals(Vec<signal_hook::SigId>);
impl Drop for Signals {
    fn drop(&mut self) {
        for id in self.0.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

pub fn run(db: &Db, options: Options) -> Result<()> {
    ensure!(
        tui::available(),
        "A interface precisa de um terminal interativo. Use `ui` no terminal ou os comandos com --help para scripts."
    );
    let cwd = options.cwd.clone();
    let view = ui_data::load(
        options.page,
        db,
        &options.config,
        &cwd,
        options.timezone,
        Period::Day,
    )?;
    let page_index = PAGES.iter().position(|p| *p == options.page).unwrap_or(0);
    let interrupted = Arc::new(AtomicBool::new(false));
    let mut app = App {
        options,
        cwd,
        executable: std::env::current_exe().context("Executável do StackPulse indisponível")?,
        page_index,
        menu_focus: true,
        period: Period::Day,
        view,
        selected: 0,
        scroll: 0,
        notice: String::new(),
        mode: Mode::Browse,
        job: None,
        interrupted: interrupted.clone(),
    };
    let mut signals = Signals(vec![]);
    for signal in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
        signal_hook::consts::SIGINT,
    ] {
        signals
            .0
            .push(signal_hook::flag::register(signal, interrupted.clone())?);
    }
    let mut guard = Some(TerminalGuard::enter()?);
    loop {
        if interrupted.load(Ordering::Relaxed) {
            break;
        }
        app.poll_job(db);
        let (columns, rows) = terminal::size()?;
        app.frame(columns, rows)
            .draw(columns, rows, tui::colors())?;
        if !event::poll(Duration::from_millis(if app.job.is_some() {
            200
        } else {
            1000
        }))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) => {
                if (columns < 76 || rows < 24)
                    && !matches!(key.code, KeyCode::Esc)
                    && !(key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL))
                {
                    continue;
                }
                if app.key(key, db, &mut guard)? {
                    break;
                }
            }
            Event::Paste(value) if columns >= 76 && rows >= 24 => app.paste(&value),
            _ => {}
        }
    }
    drop(app.job.take());
    drop(guard);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(db: &Db, dir: &Path, page: Page) -> App {
        let options = Options {
            no_policy: false,
            cwd: dir.into(),
            db: dir.join("usage.sqlite"),
            config: dir.join("missing.json"),
            sessions: dir.join("sessions"),
            timezone: chrono_tz::UTC,
            page,
        };
        App {
            view: ui_data::load(
                page,
                db,
                &options.config,
                dir,
                options.timezone,
                Period::Day,
            )
            .unwrap(),
            options,
            cwd: dir.into(),
            executable: "/not-a-provider".into(),
            page_index: PAGES.iter().position(|p| *p == page).unwrap(),
            menu_focus: true,
            period: Period::Day,
            selected: 0,
            scroll: 0,
            notice: String::new(),
            mode: Mode::Browse,
            job: None,
            interrupted: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn browse_to_record_and_annotation_keeps_exact_id_without_running_a_client() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&dir.path().join("usage.sqlite")).unwrap();
        let data = serde_json::from_str(include_str!("../examples/execution.json")).unwrap();
        db.ingest(&data).unwrap();
        let mut app = app(&db, dir.path(), Page::Runs);
        let mut guard = None;
        app.key(KeyCode::Enter.into(), &db, &mut guard).unwrap();
        let Mode::Detail(doc) = &app.mode else {
            panic!("expected run details")
        };
        assert!(doc.lines.iter().any(|s| s.contains("demo-root")));
        app.key(KeyCode::Esc.into(), &db, &mut guard).unwrap();
        app.key(KeyCode::Char('t').into(), &db, &mut guard).unwrap();
        let Mode::Form(editor) = &app.mode else {
            panic!("expected annotation form")
        };
        assert_eq!(editor.inputs[0].value, "demo-root");
        assert!(app.job.is_none());
        assert!(!app.options.config.exists());
    }

    #[test]
    fn oversized_paste_preserves_entire_prompt_and_cancel_does_not_launch_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("usage.sqlite")).unwrap();
        let mut app = app(&db, dir.path(), Page::Run);
        let mut guard = None;
        app.key(KeyCode::Enter.into(), &db, &mut guard).unwrap();
        let prompt = "ação\n\t$(comando literal)";
        app.paste(prompt);
        app.paste(&"x".repeat(5000));
        let Mode::Form(editor) = &app.mode else {
            panic!("expected prompt form")
        };
        assert_eq!(editor.inputs[0].value, prompt);
        assert!(editor.error.contains("preservado"));
        app.key(KeyCode::Esc.into(), &db, &mut guard).unwrap();
        assert!(matches!(app.mode, Mode::Browse));
        assert!(db.executions().unwrap().is_empty());
        assert!(app.job.is_none());
    }

    #[test]
    fn feedback_correlation_requires_cli_record_marker_and_globals_keep_literal_paths() {
        let id = "b1a19775-a1a3-49dc-8295-5d1b08af5de9";
        assert_eq!(
            emitted_execution_id(&[format!("Registro {id}"), format!("stderr: unrelated {id}")]),
            None
        );
        assert_eq!(
            emitted_execution_id(&[format!(
                "stderr: AI Timeline · equipe · modelo · registro {id}"
            )]),
            Some(id.into())
        );
        assert_eq!(
            path_option("db", Path::new("-hist ação.sqlite")),
            OsString::from("--db=-hist ação.sqlite")
        );
    }
    #[test]
    fn wrapping_preserves_lines_unicode_and_filters_terminal_controls() {
        assert_eq!(
            wrapped(&["á界x\n\u{1b}ok".into()], 3),
            vec!["á界", "x", "ok"]
        );
        assert!(wrapped(&["x".into()], 0).is_empty());
    }
    #[test]
    fn multiline_paste_is_only_for_prompt_fields() {
        let mut editor = Editor::new(ui_commands::form(Action::Run, None));
        let i = editor
            .form
            .fields
            .iter()
            .position(|f| f.multiline)
            .expect("pedido multilinha");
        editor.inputs[i].insert_multiline("primeiro\nsegundo");
        assert!(editor.inputs[i].value.contains('\n'));
    }
}
