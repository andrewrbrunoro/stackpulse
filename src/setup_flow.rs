//! Visual application of setup choices, including progress, recovery and completion.
use crate::{
    memory,
    profiles::Settings,
    tui::{self, Frame, TerminalGuard, Tone},
    ui_job::Job,
    usagebar,
};
use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal,
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use unicode_width::UnicodeWidthChar;

pub use crate::tui::available;

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Done,
    Review,
    Failed,
}

pub fn finish_setup(settings: &Settings, config: &Path, cwd: &Path) -> Result<Outcome> {
    let mut packages = vec![];
    if settings.ai_memory {
        packages.push(Package::Memory);
    }
    if settings.ai_usagebar {
        packages.push(Package::Usagebar);
    }
    run(Some((settings, config)), None, cwd, packages)
}

pub fn install(prefix: &Path, cwd: &Path) -> Result<Outcome> {
    run(None, Some(prefix), cwd, vec![Package::Memory])
}

pub fn install_usagebar(prefix: &Path, cwd: &Path) -> Result<Outcome> {
    run(None, Some(prefix), cwd, vec![Package::Usagebar])
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Package {
    Memory,
    Usagebar,
}
impl Package {
    fn name(self) -> &'static str {
        match self {
            Self::Memory => "AI-Memory",
            Self::Usagebar => "AI-UsageBar",
        }
    }
    fn command(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Usagebar => "usagebar",
        }
    }
    fn url(self) -> &'static str {
        match self {
            Self::Memory => memory::PROJECT_URL,
            Self::Usagebar => usagebar::PROJECT_URL,
        }
    }
    fn note(self) -> &'static str {
        match self {
            Self::Memory => "Conexão aos agentes é uma etapa separada.",
            Self::Usagebar => "CLI/TUI · barra gráfica configurada separadamente.",
        }
    }
}

struct Flow<'a> {
    setup: Option<(&'a Settings, &'a Path)>,
    prefix: Option<&'a Path>,
    cwd: &'a Path,
    executable: PathBuf,
    job: Option<Job>,
    saved: bool,
    complete: bool,
    cancelled: bool,
    error: Option<String>,
    output: Vec<String>,
    history: Vec<String>,
    packages: Vec<Package>,
    next_package: usize,
    selected: usize,
    scroll: usize,
    elapsed: Duration,
}

impl Flow<'_> {
    fn current_package(&self) -> Option<Package> {
        self.packages
            .get(self.next_package)
            .or_else(|| self.packages.last())
            .copied()
    }

    fn attempt(&mut self) {
        self.error = None;
        self.complete = false;
        self.cancelled = false;
        self.output.clear();
        self.selected = 0;
        self.scroll = 0;
        self.elapsed = Duration::ZERO;
        let result = (|| -> Result<()> {
            if !self.saved
                && let Some((settings, config)) = self.setup
            {
                std::fs::create_dir_all(settings.providers_root.join("project/all"))?;
                settings.save(config)?;
                self.saved = true;
            }
            let Some(package) = self.packages.get(self.next_package) else {
                self.complete = true;
                return Ok(());
            };
            if *package == Package::Usagebar {
                anyhow::ensure!(usagebar::supported(), usagebar::platform_description());
            }
            let prefix = self
                .prefix
                .map(Path::to_owned)
                .map(Ok)
                .unwrap_or_else(memory::default_prefix)?;
            let args: Vec<OsString> = vec![
                "--allow-workspace".into(),
                self.cwd.into(),
                package.command().into(),
                "install".into(),
                "--prefix".into(),
                prefix.into(),
            ];
            // Redirected output keeps the child in its noninteractive installation path.
            self.job = Some(Job::start(&self.executable, &args, self.cwd, None)?);
            Ok(())
        })();
        if let Err(error) = result {
            self.error = Some(format!("{error:#}"));
        }
    }

    fn poll(&mut self) {
        let Some(job) = &mut self.job else {
            return;
        };
        self.elapsed = job.elapsed();
        self.output = job.lines();
        match job.poll() {
            Ok(Some(result)) => {
                self.output = job.lines();
                self.elapsed = job.elapsed();
                self.cancelled = result.cancelled;
                if !result.success {
                    self.error = Some(result.message);
                }
                self.job = None;
                if result.success {
                    self.history.append(&mut self.output);
                    self.next_package += 1;
                    self.attempt();
                }
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                self.job = None;
            }
            Ok(None) => {}
        }
    }

    fn actions(&self) -> Vec<(&'static str, Outcome)> {
        let mut actions = vec![];
        if !self.complete {
            actions.push(("Tentar novamente", Outcome::Failed));
        }
        actions.push(("Concluir", Outcome::Done));
        if self.setup.is_some() {
            actions.push(("Voltar ao setup", Outcome::Review));
        }
        actions
    }

    fn lines(&self, width: usize) -> Vec<String> {
        let mut lines = vec![];
        if let Some((settings, config)) = self.setup {
            lines.push(format!("CLI       {}", settings.client.label()));
            lines.push(format!(
                "Modelo    {} · {}",
                settings.model, settings.effort
            ));
            lines.push(format!(
                "Perfis    {}",
                tui::display_path(&settings.providers_root)
            ));
            lines.push(format!("Setup     {}", tui::display_path(config)));
            if !settings.ai_memory {
                lines.push("AI-Memory não selecionado.".into());
            }
            if usagebar::supported() && !settings.ai_usagebar {
                lines.push("AI-UsageBar não selecionado.".into());
            }
        }
        if let Some(error) = &self.error {
            lines.push(format!("Atenção: {error}"));
        }
        lines.extend(
            self.history
                .iter()
                .chain(self.output.iter())
                .filter(|line| {
                    !line.contains(memory::PROJECT_URL)
                        && !line.contains(usagebar::PROJECT_URL)
                        && !line.contains(memory::CREATOR_URL)
                        && !line.starts_with("AI-Memory · criado por")
                })
                .cloned(),
        );
        if self.job.is_some()
            && self.output.is_empty()
            && let Some(package) = self.current_package()
        {
            lines.push(format!(
                "Verificando a instalação existente do {}…",
                package.name()
            ));
        }
        lines
            .into_iter()
            .flat_map(|line| wrap(&line, width))
            .collect()
    }

    fn frame(&mut self, columns: u16, rows: u16) -> Frame {
        let width = usize::from(columns.min(94));
        let height = usize::from(rows.min(30));
        let mut f = Frame::blank(width, height);
        f.content(1, "✦ STACKPULSE", Tone::Accent);
        if columns < 56 || rows < 20 {
            f.content(3, "Amplie o terminal para 56 × 20.", Tone::Warning);
            f.content(5, "Esc / Ctrl+C para encerrar.", Tone::Muted);
            return f;
        }
        f.content(
            2,
            if self.setup.is_some() {
                "SETUP · APLICAR CONFIGURAÇÃO"
            } else {
                "SETUP · INSTALAÇÃO OPCIONAL"
            },
            Tone::Muted,
        );
        let running = self.job.is_some();
        let preparing = format!(
            "Preparando {}…",
            self.current_package().map_or("opcionais", Package::name)
        );
        let title = if running {
            if self.job.as_ref().is_some_and(Job::cancelling) {
                "Encerrando instalação…"
            } else {
                &preparing
            }
        } else if self.cancelled {
            "Instalação interrompida"
        } else if self.complete {
            "Tudo pronto"
        } else {
            "A instalação precisa de atenção"
        };
        f.content(
            4,
            title,
            if self.error.is_some() {
                Tone::Warning
            } else {
                Tone::Accent
            },
        );
        let status = if running {
            format!(
                "{}  Etapa {}/{} · {}s",
                ["◐", "◓", "◑", "◒"][(self.elapsed.as_millis() / 150) as usize % 4],
                self.next_package + 1,
                self.packages.len(),
                self.elapsed.as_secs()
            )
        } else if self.saved {
            "✓ Configuração salva".into()
        } else if self.complete {
            "✓ Pacote disponível".into()
        } else {
            "Consulte o motivo abaixo e escolha uma ação.".into()
        };
        f.content(5, &status, Tone::Text);
        f.content(6, &"─".repeat(width - 6), Tone::Muted);
        let capacity = height - 17;
        let lines = self.lines(width - 6);
        self.scroll = self.scroll.min(lines.len().saturating_sub(capacity));
        for (i, line) in lines.iter().skip(self.scroll).take(capacity).enumerate() {
            f.content(7 + i, line, Tone::Text);
        }
        if lines.len() > capacity {
            f.content(
                height - 10,
                &format!(
                    "{}–{} de {} · PgUp/PgDn detalhes",
                    self.scroll + 1,
                    (self.scroll + capacity).min(lines.len()),
                    lines.len()
                ),
                Tone::Muted,
            );
        }
        if let Some(package) = self.current_package() {
            f.content(
                height - 9,
                &format!("{} · Fabio Akita (AkitaOnRails)", package.name()),
                Tone::Muted,
            );
            f.content(height - 8, package.url(), Tone::Accent);
            f.content(height - 7, memory::CREATOR_URL, Tone::Muted);
            f.content(height - 6, package.note(), Tone::Muted);
        }
        if running {
            f.content(height - 4, "[ Esc  Cancelar instalação ]", Tone::Selected);
            f.content(
                height - 2,
                "Acompanhe o progresso · Esc / Ctrl+C cancelar",
                Tone::Muted,
            );
        } else {
            for (i, (label, _)) in self.actions().iter().enumerate() {
                f.content(
                    height - 5 + i,
                    &format!("{} {label}", if i == self.selected { "›" } else { " " }),
                    if i == self.selected {
                        Tone::Selected
                    } else {
                        Tone::Text
                    },
                );
            }
            f.content(
                height - 1,
                "↑↓ / Tab escolher · Enter confirmar · Esc sair",
                Tone::Muted,
            );
        }
        f
    }
}

fn run(
    setup: Option<(&Settings, &Path)>,
    prefix: Option<&Path>,
    cwd: &Path,
    packages: Vec<Package>,
) -> Result<Outcome> {
    let interrupted = Arc::new(AtomicBool::new(false));
    let mut signals = Signals(vec![]);
    for signal in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ] {
        signals
            .0
            .push(signal_hook::flag::register(signal, interrupted.clone())?);
    }
    let _guard = TerminalGuard::enter()?;
    let mut flow = Flow {
        setup,
        prefix,
        cwd,
        executable: std::env::current_exe().context("Executável do StackPulse indisponível")?,
        job: None,
        saved: false,
        complete: false,
        cancelled: false,
        error: None,
        output: vec![],
        history: vec![],
        packages,
        next_package: 0,
        selected: 0,
        scroll: 0,
        elapsed: Duration::ZERO,
    };
    flow.attempt();
    let mut redraw = true;
    loop {
        let was_running = flow.job.is_some();
        flow.poll();
        if interrupted.load(Ordering::Relaxed) {
            if let Some(job) = &mut flow.job {
                job.cancel();
            } else {
                return Ok(Outcome::Failed);
            }
        }
        let (columns, rows) = terminal::size()?;
        if redraw || was_running {
            flow.frame(columns, rows)
                .draw(columns, rows, tui::colors())?;
            redraw = false;
        }
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let event = event::read()?;
        redraw = true;
        if let Event::Key(key) = event {
            if key.kind == KeyEventKind::Release {
                continue;
            }
            let cancel = key.code == KeyCode::Esc
                || (key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL));
            if cancel {
                if let Some(job) = &mut flow.job {
                    job.cancel();
                } else {
                    return Ok(if flow.complete {
                        Outcome::Done
                    } else {
                        Outcome::Failed
                    });
                }
                continue;
            }
            match key.code {
                KeyCode::PageDown => flow.scroll = flow.scroll.saturating_add(5),
                KeyCode::PageUp => flow.scroll = flow.scroll.saturating_sub(5),
                _ => {}
            }
            if flow.job.is_some() || columns < 56 || rows < 20 {
                continue;
            }
            let count = flow.actions().len();
            match key.code {
                KeyCode::Tab | KeyCode::Down => flow.selected = (flow.selected + 1) % count,
                KeyCode::BackTab | KeyCode::Up => {
                    flow.selected = (flow.selected + count - 1) % count
                }
                KeyCode::Enter => match flow.actions().remove(flow.selected).1 {
                    Outcome::Failed => flow.attempt(),
                    Outcome::Done => {
                        return Ok(if flow.complete {
                            Outcome::Done
                        } else {
                            Outcome::Failed
                        });
                    }
                    Outcome::Review => return Ok(Outcome::Review),
                },
                _ => {}
            }
        }
    }
}

struct Signals(Vec<signal_hook::SigId>);
impl Drop for Signals {
    fn drop(&mut self) {
        for id in self.0.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

fn wrap(value: &str, width: usize) -> Vec<String> {
    let value = crate::widget::clean(value, 4096);
    let mut result = vec![];
    let mut line = String::new();
    let mut columns = 0;
    for ch in value.chars() {
        let count = ch.width().unwrap_or(0);
        if columns + count > width && !line.is_empty() {
            result.push(std::mem::take(&mut line));
            columns = 0;
        }
        line.push(ch);
        columns += count;
    }
    if !line.is_empty() {
        result.push(line);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    fn test_flow(cwd: &Path, packages: Vec<Package>) -> Flow<'_> {
        Flow {
            setup: None,
            prefix: Some(cwd),
            cwd,
            executable: cwd.join("installer"),
            job: None,
            saved: false,
            complete: false,
            cancelled: false,
            error: None,
            output: vec![],
            history: vec![],
            packages,
            next_package: 0,
            selected: 0,
            scroll: 0,
            elapsed: Duration::ZERO,
        }
    }

    #[test]
    fn no_selected_packages_finishes_without_starting_a_process() {
        let temp = tempfile::tempdir().unwrap();
        let mut flow = test_flow(temp.path(), vec![]);
        flow.attempt();
        assert!(flow.complete && flow.job.is_none() && flow.error.is_none());
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    #[test]
    #[cfg(unix)]
    fn retry_resumes_failed_package_and_preserves_completed_package_output() {
        if !usagebar::supported() {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let mut flow = test_flow(temp.path(), vec![Package::Memory, Package::Usagebar]);
        std::fs::write(&flow.executable, "#!/bin/sh\nprintf '%s\\n' \"$3\" >> calls\nprintf '%s disponível\\n' \"$3\"\nif [ \"$3\" = usagebar ] && [ ! -f retry ]; then exit 1; fi\n").unwrap();
        std::fs::set_permissions(&flow.executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        fn settle(flow: &mut Flow<'_>) {
            let start = std::time::Instant::now();
            while flow.job.is_some() {
                assert!(start.elapsed() < Duration::from_secs(5));
                flow.poll();
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        flow.attempt();
        settle(&mut flow);
        assert!(!flow.complete && flow.error.is_some());
        assert_eq!(flow.next_package, 1);
        assert_eq!(
            std::fs::read_to_string(temp.path().join("calls")).unwrap(),
            "memory\nusagebar\n"
        );
        assert!(
            flow.lines(80)
                .iter()
                .any(|line| line.contains("memory disponível"))
        );
        std::fs::write(temp.path().join("retry"), "").unwrap();
        flow.attempt();
        settle(&mut flow);
        assert!(flow.complete && flow.error.is_none());
        assert_eq!(flow.next_package, 2);
        assert_eq!(
            std::fs::read_to_string(temp.path().join("calls")).unwrap(),
            "memory\nusagebar\nusagebar\n"
        );
        assert!(
            flow.lines(80)
                .iter()
                .any(|line| line.contains("memory disponível"))
        );
        assert!(
            flow.lines(80)
                .iter()
                .any(|line| line.contains("usagebar disponível"))
        );
    }

    #[test]
    fn failure_details_remain_scrollable_and_actions_fit_compact_terminal() {
        let mut flow = Flow {
            setup: None,
            prefix: Some(Path::new("/tmp/test")),
            cwd: Path::new("/tmp"),
            executable: PathBuf::new(),
            job: None,
            saved: false,
            complete: false,
            cancelled: false,
            error: Some("Erro longo com Unicode 界 ".repeat(30)),
            output: vec![],
            history: vec![],
            packages: vec![Package::Memory],
            next_package: 0,
            selected: 0,
            scroll: 0,
            elapsed: Duration::ZERO,
        };
        for (w, h) in [(56, 20), (56, 24), (94, 30), (40, 10)] {
            let frame = flow.frame(w, h);
            for span in frame.spans {
                assert!(span.y < usize::from(h));
                assert!(span.x + span.text.width() <= usize::from(w));
            }
        }
        flow.scroll = usize::MAX;
        flow.frame(56, 24);
        assert!(flow.scroll > 0 && flow.scroll < usize::MAX);
        assert_eq!(flow.actions()[0].0, "Tentar novamente");
        flow.complete = true;
        assert_eq!(flow.actions()[0].0, "Concluir");
    }
}
