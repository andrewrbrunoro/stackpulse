//! Keyboard-driven setup. The wizard edits a draft; its caller owns persistence.
use crate::{
    cli_providers::{self, Inventory},
    client::Backend,
    profiles::Settings,
    tui::{Frame, Input, TerminalGuard, Tone, colors, display_path},
    widget::clean,
};
use anyhow::{Context, Result, ensure};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    queue,
    style::{Color, ResetColor, SetForegroundColor},
    terminal,
};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
};
use unicode_width::UnicodeWidthStr;

pub use crate::tui::available;

struct Wizard<'a> {
    inventory: &'a Inventory,
    original: &'a Settings,
    config_path: &'a Path,
    cwd: PathBuf,
    step: usize,
    selected: usize,
    field: usize,
    fields: Vec<Input>,
    ai_memory: bool,
    ai_usagebar: bool,
    usagebar_supported: bool,
    optional_selected: usize,
    active_client: Backend,
    client_drafts: Vec<(Backend, Vec<Input>)>,
    error: Option<String>,
}

impl<'a> Wizard<'a> {
    fn new(settings: &'a Settings, inventory: &'a Inventory, path: &'a Path) -> Result<Self> {
        Self::new_with_usagebar_support(settings, inventory, path, crate::usagebar::supported())
    }

    fn new_with_usagebar_support(
        settings: &'a Settings,
        inventory: &'a Inventory,
        path: &'a Path,
        usagebar_supported: bool,
    ) -> Result<Self> {
        let selected = inventory
            .installed
            .iter()
            .position(|cli| {
                cli.adapter_available
                    && (Backend::from_cli_id(&cli.id) == Some(settings.client)
                        || cli.id == "configured")
                    && std::iter::once(&cli.executable)
                        .chain(&cli.other_installations)
                        .any(|p| cli_providers::same_executable(p, &settings.executable))
            })
            .or_else(|| {
                inventory
                    .installed
                    .iter()
                    .position(|cli| cli.adapter_available)
            })
            .unwrap_or(0);
        Ok(Self {
            inventory,
            original: settings,
            config_path: path,
            cwd: std::env::current_dir()?,
            step: 0,
            selected,
            field: 0,
            ai_memory: settings.ai_memory,
            ai_usagebar: settings.ai_usagebar && usagebar_supported,
            usagebar_supported,
            optional_selected: 0,
            error: None,
            active_client: settings.client,
            client_drafts: Vec::new(),
            fields: [
                settings.provider.clone(),
                settings.model.clone(),
                settings.effort.clone(),
                settings.providers_root.to_string_lossy().into_owned(),
                settings.default_profile.clone().unwrap_or_default(),
                settings.max_agents.to_string(),
            ]
            .into_iter()
            .map(Input::new)
            .collect(),
        })
    }
    fn selected_client(&self) -> Result<Backend> {
        let cli = self
            .inventory
            .installed
            .get(self.selected)
            .context("Nenhum CLI disponível. Instale um CLI compatível para continuar.")?;
        ensure!(
            cli.adapter_available,
            "{} está instalado, mas ainda não tem adaptador no StackPulse.",
            cli.name
        );
        Backend::from_cli_id(&cli.id)
            .or_else(|| (cli.id == "configured").then_some(self.original.client))
            .context("Não foi possível identificar o client deste executável.")
    }
    fn activate_selected_client(&mut self) -> Result<()> {
        let client = self.selected_client()?;
        if client == self.active_client {
            return Ok(());
        }
        let draft = self.fields[..3].to_vec();
        if let Some((_, values)) = self
            .client_drafts
            .iter_mut()
            .find(|(backend, _)| *backend == self.active_client)
        {
            *values = draft;
        } else {
            self.client_drafts.push((self.active_client, draft));
        }
        let next = self
            .client_drafts
            .iter()
            .find(|(backend, _)| *backend == client)
            .map(|(_, values)| values.clone())
            .unwrap_or_else(|| {
                [
                    client.default_provider(),
                    client.default_model(),
                    client.default_effort(),
                ]
                .into_iter()
                .map(|value| Input::new(value.into()))
                .collect()
            });
        self.fields[..3].clone_from_slice(&next);
        self.active_client = client;
        Ok(())
    }
    fn settings(&self) -> Result<Settings> {
        let client = self.selected_client()?;
        let cli = self
            .inventory
            .installed
            .get(self.selected)
            .context("Nenhum CLI disponível. Instale um CLI compatível para continuar.")?;
        let value = |n: usize| self.fields[n].value.trim().to_string();
        let root = value(3);
        ensure!(!root.is_empty(), "Informe uma pasta para os perfis.");
        let root = if root == "~" || root.starts_with("~/") {
            PathBuf::from(
                std::env::var_os("HOME").context("HOME indisponível; use um caminho absoluto.")?,
            )
            .join(root.strip_prefix("~/").unwrap_or(""))
        } else {
            PathBuf::from(root)
        };
        let executable = if client == self.original.client
            && std::iter::once(&cli.executable)
                .chain(&cli.other_installations)
                .any(|p| cli_providers::same_executable(p, &self.original.executable))
        {
            self.original.executable.clone()
        } else {
            cli.executable.clone()
        };
        let settings = Settings {
            schema_version: 1,
            client,
            provider: value(0),
            model: value(1),
            effort: value(2),
            executable,
            providers_root: if root.is_absolute() {
                root
            } else {
                self.cwd.join(root)
            },
            default_profile: (!value(4).is_empty()).then(|| value(4)),
            max_agents: value(5)
                .parse()
                .context("Concorrência: informe um número de 1 a 100.")?,
            ai_memory: self.ai_memory,
            ai_usagebar: self.ai_usagebar && self.usagebar_supported,
        };
        settings.validate()?;
        Ok(settings)
    }
    fn advance(&mut self) -> Result<Option<Settings>> {
        if self.step == 0 {
            self.activate_selected_client()?;
            self.step = 1;
        } else if self.field < 2 && self.step < 3 {
            self.field += 1;
        } else {
            let settings = if self.step == 1 {
                // A user returning from Project must be able to fix its fields later.
                let mut settings = self.original.clone();
                settings.client = self.active_client;
                settings.provider = self.fields[0].value.trim().into();
                settings.model = self.fields[1].value.trim().into();
                settings.effort = self.fields[2].value.trim().into();
                settings.validate()?;
                settings
            } else {
                self.settings()?
            };
            if self.step == 4 {
                return Ok(Some(settings));
            }
            self.step += 1;
            self.field = 0;
        }
        Ok(None)
    }
    fn key(&mut self, key: KeyEvent) -> Result<Option<Settings>> {
        self.error = None;
        match key.code {
            KeyCode::Esc | KeyCode::F(2) => {
                self.step = self.step.saturating_sub(1);
                self.field = 0;
            }
            KeyCode::Enter => return self.advance(),
            KeyCode::F(5) => {
                let previous_field = self.field;
                if (1..=2).contains(&self.step) {
                    self.field = 2;
                }
                let result = self.advance();
                if result.is_err() {
                    self.field = previous_field;
                }
                return result;
            }
            KeyCode::Char(' ') if self.step == 3 => {
                if self.optional_selected == 1 && self.usagebar_supported {
                    self.ai_usagebar = !self.ai_usagebar;
                } else {
                    self.ai_memory = !self.ai_memory;
                }
            }
            KeyCode::Up | KeyCode::BackTab | KeyCode::Down | KeyCode::Tab if self.step == 3 => {
                self.optional_selected = if self.usagebar_supported {
                    1 - self.optional_selected
                } else {
                    0
                };
            }
            KeyCode::Up | KeyCode::BackTab if self.step == 0 => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Tab if self.step == 0 => {
                self.selected =
                    (self.selected + 1).min(self.inventory.installed.len().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::BackTab if self.step < 3 => self.field = (self.field + 2) % 3,
            KeyCode::Down | KeyCode::Tab if self.step < 3 => self.field = (self.field + 1) % 3,
            _ if (1..=2).contains(&self.step) => {
                self.fields[(self.step - 1) * 3 + self.field].key(key)
            }
            _ => {}
        }
        Ok(None)
    }
    fn frame(&self, width: usize, height: usize) -> Frame {
        let mut frame = Frame::new(width, height);
        frame.content(1, "✦ STACKPULSE", Tone::Accent);
        let progress = format!("ETAPA {}/5 · {}%", self.step + 1, (self.step + 1) * 20);
        frame.put(width - 3 - progress.width(), 1, &progress, Tone::Muted);
        frame.content(
            2,
            &format!("Projeto · {}", display_path(&self.cwd)),
            Tone::Muted,
        );
        let labels = if width < 76 {
            ["CLI", "Modelo", "Projeto", "Opções", "Revisar"]
        } else {
            ["CLI", "Auxiliar", "Projeto", "Opcionais", "Revisão"]
        };
        let progress_width = width - 6;
        let segment_width = progress_width / labels.len();
        for (i, label) in labels.iter().enumerate() {
            let x = 3 + i * segment_width;
            frame.put(
                x,
                4,
                &format!(
                    "{} {label}",
                    if i == self.step {
                        "›"
                    } else if i < self.step {
                        "✓"
                    } else {
                        "·"
                    }
                ),
                if i == self.step {
                    Tone::Selected
                } else if i < self.step {
                    Tone::Accent
                } else {
                    Tone::Muted
                },
            );
        }
        frame.content(5, &"─".repeat(progress_width), Tone::Muted);
        frame.content(
            5,
            &"━".repeat(progress_width * (self.step + 1) / labels.len()),
            Tone::Accent,
        );
        let (title, subtitle) = match self.step {
            0 => (
                "Escolha o CLI que executará o auxiliar",
                match self.inventory.installed.len() {
                    1 => "1 CLI detectado · a autenticação existente será reutilizada".into(),
                    count => format!("{count} CLIs detectados · principais primeiro"),
                },
            ),
            1 => (
                "Configure o modelo auxiliar",
                "Ele transforma imagens em perfis para a sua equipe.".into(),
            ),
            2 => (
                "Defina perfis e capacidade",
                "Escolha onde ficam os perfis e quantos agentes podem atuar.".into(),
            ),
            3 => (
                "Escolha os complementos",
                "Marque somente os recursos que deseja preparar ao salvar.".into(),
            ),
            _ => (
                "Confirme a configuração",
                format!("Será salva em {}", display_path(self.config_path)),
            ),
        };
        frame.content(6, title, Tone::Text);
        frame.content(7, &subtitle, Tone::Muted);
        match self.step {
            0 => {
                let slots = (height - 17) / 2;
                let start = self.selected.saturating_sub(slots - 1);
                for (i, cli) in self
                    .inventory
                    .installed
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(slots)
                {
                    let y = 9 + (i - start) * 2;
                    let selected = i == self.selected;
                    let status = if cli.adapter_available {
                        "● PRONTO"
                    } else {
                        "○ SEM ADAPTADOR"
                    };
                    let name = clean(&cli.name, width - 10 - status.width());
                    let row = format!("{} {}", if selected { "›" } else { " " }, name);
                    frame.content(
                        y,
                        &format!(
                            "{row}{}",
                            " ".repeat((width - 6).saturating_sub(row.width()))
                        ),
                        if selected { Tone::Selected } else { Tone::Text },
                    );
                    frame.put(
                        width - 3 - status.width(),
                        y,
                        status,
                        if selected {
                            Tone::Selected
                        } else if cli.adapter_available {
                            Tone::Accent
                        } else {
                            Tone::Muted
                        },
                    );
                    frame.put(
                        5,
                        y + 1,
                        &clean(&format!("↳ {}", display_path(&cli.executable)), width - 9),
                        Tone::Muted,
                    );
                }
                if self.inventory.installed.is_empty() {
                    frame.content(10, "Nenhum CLI do catálogo foi encontrado.", Tone::Warning);
                    frame.content(
                        12,
                        "Instale um CLI compatível e reabra o setup.",
                        Tone::Text,
                    );
                }
                if let Some(cli) = self.inventory.installed.get(self.selected) {
                    frame.content(
                        height - 8,
                        if cli.adapter_available {
                            "Credenciais · usa a autenticação já configurada neste CLI"
                        } else {
                            "Indisponível · este CLI ainda não possui adaptador"
                        },
                        if cli.adapter_available {
                            Tone::Muted
                        } else {
                            Tone::Warning
                        },
                    );
                    if let Some(note) = &cli.note {
                        frame.content(height - 7, note, Tone::Muted);
                    }
                }
                if self.inventory.installed.len() > slots {
                    frame.content(
                        height - 6,
                        &format!(
                            "Item {} de {} · ↑ ↓ para percorrer",
                            self.selected + 1,
                            self.inventory.installed.len()
                        ),
                        Tone::Muted,
                    );
                }
            }
            1 | 2 => {
                let provider_label = format!("Provider em {}", self.active_client.label());
                let labels = [
                    provider_label.as_str(),
                    "Modelo com visão",
                    "Esforço de raciocínio",
                    "Pasta de perfis",
                    "Perfil padrão (opcional)",
                    "Subagentes simultâneos",
                ];
                let (provider_hint, model_hint, effort_hint) = match self.active_client {
                    Backend::Codex => (
                        "openai ou outro provider configurado no Codex",
                        "Identificador de um modelo com visão no CLI",
                        "default, low, medium, high, xhigh, max ou ultra",
                    ),
                    Backend::Claude => (
                        "anthropic · usa sua conta do Claude Code",
                        "default usa o CLI; ou um modelo com visão",
                        "default, low, medium, high, xhigh ou max",
                    ),
                    Backend::Cursor => (
                        "cursor · usa sua conta do Cursor",
                        "default usa o CLI; ou um modelo com visão",
                        "default usa o CLI; esforço exige modelo explícito",
                    ),
                    Backend::Grok => (
                        "xai · usa sua conta do Grok CLI",
                        "default usa o CLI; ou um modelo com visão",
                        "default, none, low, medium, high ou max",
                    ),
                };
                let hints = [
                    provider_hint,
                    model_hint,
                    effort_hint,
                    "Imagens: project/all ou project/nome-do-projeto",
                    "Vazio: automático quando houver só um perfil",
                    "1 a 100; delega apenas conforme a necessidade",
                ];
                for i in 0..3 {
                    let index = (self.step - 1) * 3 + i;
                    let y = 9 + i * 3;
                    let active = self.field == i;
                    let field_label = format!("{} · campo {} de 3", labels[index], i + 1);
                    frame.content(
                        y,
                        &field_label,
                        if active { Tone::Accent } else { Tone::Muted },
                    );
                    if active && width >= 76 {
                        let editing = "EDITANDO";
                        frame.put(width - 3 - editing.width(), y, editing, Tone::Accent);
                    }
                    let (value, caret) = self.fields[index].visible(width - 11);
                    let shown = if value.is_empty() && !active && index == 4 {
                        "automático".into()
                    } else {
                        value
                    };
                    let value = format!("{} {shown}", if active { "›" } else { " " });
                    frame.content(
                        y + 1,
                        &format!(
                            "{value}{}",
                            " ".repeat((width - 6).saturating_sub(value.width()))
                        ),
                        if active { Tone::Selected } else { Tone::Text },
                    );
                    if active {
                        frame.caret = Some((5 + caret, y + 1));
                    }
                    frame.content(y + 2, &format!("  ↳ {}", hints[index]), Tone::Muted);
                }
            }
            3 => {
                let options = [
                    ("AI-Memory", self.ai_memory),
                    ("AI-UsageBar", self.ai_usagebar),
                ];
                for (i, (name, checked)) in options
                    .iter()
                    .take(if self.usagebar_supported { 2 } else { 1 })
                    .enumerate()
                {
                    let tone = if i == self.optional_selected {
                        Tone::Selected
                    } else {
                        Tone::Text
                    };
                    if i == self.optional_selected {
                        frame.content(9 + i, &" ".repeat(width - 6), tone);
                    }
                    frame.content(
                        9 + i,
                        &format!("[{}] Instalar {name}", if *checked { "x" } else { " " }),
                        tone,
                    );
                    if width >= 76 {
                        let state = if *checked { "INCLUIR" } else { "IGNORAR" };
                        frame.put(width - 3 - state.width(), 9 + i, state, tone);
                    }
                }
                let usagebar = self.optional_selected == 1 && self.usagebar_supported;
                if usagebar {
                    frame.content(12, "AI-UsageBar · consumo dos CLIs no terminal", Tone::Text);
                    frame.content(13, "Barra gráfica: configure separadamente.", Tone::Muted);
                    frame.content(14, &crate::usagebar::platform_description(), Tone::Muted);
                } else {
                    frame.content(12, "AI-Memory · memória de longo prazo", Tone::Text);
                    frame.content(13, "Conexão aos agentes: etapa separada.", Tone::Muted);
                    frame.content(14, "Instala o pacote oficial ao salvar.", Tone::Muted);
                }
                frame.content(15, "Criado por Fabio Akita (AkitaOnRails)", Tone::Muted);
                frame.content(
                    16,
                    if usagebar {
                        crate::usagebar::PROJECT_URL
                    } else {
                        crate::memory::PROJECT_URL
                    },
                    Tone::Accent,
                );
                frame.content(17, crate::memory::CREATOR_URL, Tone::Accent);
                frame.content(
                    18,
                    if if usagebar {
                        self.ai_usagebar
                    } else {
                        self.ai_memory
                    } {
                        "Selecionado · Espaço para desmarcar"
                    } else {
                        "Dispensado · Espaço para selecionar"
                    },
                    Tone::Muted,
                );
            }
            _ => {
                if let Ok(settings) = self.settings() {
                    let cli = &self.inventory.installed[self.selected];
                    let team = settings.default_profile.unwrap_or("automática".into());
                    let agents = format!("{} simultâneos", settings.max_agents);
                    let profiles = display_path(&settings.providers_root);
                    let memory = format!(
                        "[{}] Instalar AI-Memory",
                        if settings.ai_memory { "x" } else { " " }
                    );
                    let usagebar = format!(
                        "[{}] Instalar AI-UsageBar",
                        if settings.ai_usagebar { "x" } else { " " }
                    );

                    if width >= 78 {
                        let gap = 3;
                        let column_width = (width - 6 - gap) / 2;
                        let right = 3 + column_width + gap;
                        review_heading(&mut frame, 3, 9, column_width, "AUXILIAR");
                        review_row(&mut frame, 3, 10, column_width, "CLI", &cli.name);
                        review_row(
                            &mut frame,
                            3,
                            11,
                            column_width,
                            "Provider",
                            &settings.provider,
                        );
                        review_row(&mut frame, 3, 12, column_width, "Modelo", &settings.model);
                        review_row(&mut frame, 3, 13, column_width, "Esforço", &settings.effort);

                        review_heading(&mut frame, right, 9, column_width, "PROJETO E EQUIPE");
                        review_row(&mut frame, right, 10, column_width, "Equipe", &team);
                        review_row(&mut frame, right, 11, column_width, "Agentes", &agents);
                        review_row(&mut frame, right, 12, column_width, "Perfis", &profiles);
                        review_row(
                            &mut frame,
                            right,
                            13,
                            column_width,
                            "Arquivo",
                            &display_path(self.config_path),
                        );

                        review_heading(&mut frame, 3, 15, width - 6, "COMPLEMENTOS");
                        frame.content(16, &memory, Tone::Text);
                        if self.usagebar_supported {
                            frame.put(right, 16, &usagebar, Tone::Text);
                        }
                        frame.content(
                            18,
                            "Enter ou F5 salva tudo e prepara os complementos marcados.",
                            Tone::Muted,
                        );
                    } else {
                        review_heading(&mut frame, 3, 9, width - 6, "AUXILIAR");
                        review_row(&mut frame, 3, 10, width - 6, "CLI", &cli.name);
                        review_row(&mut frame, 3, 11, width - 6, "Provider", &settings.provider);
                        review_row(&mut frame, 3, 12, width - 6, "Modelo", &settings.model);
                        review_row(&mut frame, 3, 13, width - 6, "Esforço", &settings.effort);
                        review_heading(&mut frame, 3, 14, width - 6, "PROJETO E EQUIPE");
                        review_row(&mut frame, 3, 15, width - 6, "Equipe", &team);
                        review_row(&mut frame, 3, 16, width - 6, "Agentes", &agents);
                        review_row(&mut frame, 3, 17, width - 6, "Perfis", &profiles);
                        frame.content(18, &memory, Tone::Text);
                        if self.usagebar_supported {
                            frame.put(27, 18, &usagebar, Tone::Text);
                        }
                    }
                }
            }
        }
        if let Some(error) = &self.error {
            frame.content(height - 5, &format!("! {error}"), Tone::Warning);
        }
        let back = if self.step == 0 {
            "[ Esc Sair ]"
        } else {
            "[ F2 Voltar ]"
        };
        let next = if self.step == 4 {
            "[ F5 Salvar ]"
        } else {
            "[ F5 Continuar ]"
        };
        frame.content(height - 4, back, Tone::Text);
        frame.put(width - 3 - next.width(), height - 4, next, Tone::Selected);
        let footer = match self.step {
            0 => "↑ ↓ escolher · Enter continuar · Ctrl+C cancelar",
            1 | 2 => "Tab campos · Digite para editar · Enter próximo",
            3 => "↑↓ / Tab escolher · Espaço marcar · Enter seguir",
            _ => "Enter salvar · Ctrl+C cancelar",
        };
        frame.content(height - 2, footer, Tone::Muted);
        frame
    }
}

fn review_heading(frame: &mut Frame, x: usize, y: usize, width: usize, label: &str) {
    let divider = width.saturating_sub(label.width() + 1);
    frame.put(
        x,
        y,
        &format!("{label} {}", "─".repeat(divider)),
        Tone::Accent,
    );
}

fn review_row(frame: &mut Frame, x: usize, y: usize, width: usize, label: &str, value: &str) {
    let label_width = if width < 38 { 9 } else { 10 };
    frame.put(x, y, label, Tone::Muted);
    frame.put(
        x + label_width,
        y,
        &clean(value, width.saturating_sub(label_width)),
        Tone::Text,
    );
}

pub fn run(
    settings: &Settings,
    inventory: &Inventory,
    config_path: &Path,
) -> Result<Option<Settings>> {
    let mut wizard = Wizard::new(settings, inventory, config_path)?;
    let _guard = TerminalGuard::enter()?;
    loop {
        let (columns, rows) = terminal::size()?;
        let small = columns < 56 || rows < 24;
        if small {
            let mut frame = Frame::blank(usize::from(columns), usize::from(rows));
            frame.put(
                0,
                0,
                &clean(
                    "Aumente o terminal para 56×24 ou use setup --plain.",
                    columns.saturating_sub(1).into(),
                ),
                Tone::Text,
            );
            frame.put(
                0,
                usize::from(2.min(rows.saturating_sub(1))),
                "Esc / Ctrl+C: sair",
                Tone::Text,
            );
            frame.draw(columns, rows, colors())?;
        } else {
            wizard
                .frame(usize::from(columns.min(110)), usize::from(rows.min(30)))
                .draw(columns, rows, colors())?;
        }
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
                    || (key.code == KeyCode::Esc && (wizard.step == 0 || small))
                {
                    return Ok(None);
                }
                if !small {
                    match wizard.key(key) {
                        Ok(Some(settings)) => return Ok(Some(settings)),
                        Ok(None) => {}
                        Err(error) => wizard.error = Some(error.to_string()),
                    }
                }
            }
            Event::Paste(value) if !small && (1..=2).contains(&wizard.step) => {
                let index = (wizard.step - 1) * 3 + wizard.field;
                wizard.fields[index].insert(&value);
                wizard.error = None;
            }
            _ => {}
        }
    }
}

pub fn print_saved(settings: &Settings, path: &Path) -> Result<()> {
    let width = terminal::size()
        .map_or(72, |(w, _)| usize::from(w).saturating_sub(1))
        .clamp(32, 94);
    let mut out = io::stdout().lock();
    let mut lines = vec![
        "✓  Configuração salva".to_string(),
        format!(
            "{} · {} · {}",
            settings.client.label(),
            settings.model,
            settings.effort
        ),
        format!("Perfis: {}", settings.providers_root.display()),
        format!("Setup: {}", path.display()),
        format!(
            "AI-Memory: instalação {}",
            if settings.ai_memory {
                "selecionada"
            } else {
                "dispensada"
            }
        ),
    ];
    if crate::usagebar::supported() {
        lines.push(format!(
            "AI-UsageBar: instalação {}",
            if settings.ai_usagebar {
                "selecionada"
            } else {
                "dispensada"
            }
        ));
    }
    lines.extend([
        "Conexão aos agentes: etapa separada.".into(),
        String::new(),
        "Próximo: stackpulse profiles add imagem.png".into(),
    ]);
    writeln!(out, "╭{}╮", "─".repeat(width - 2))?;
    for (i, line) in lines.iter().enumerate() {
        write!(out, "│ ")?;
        if i == 0 && colors() {
            queue!(out, SetForegroundColor(Color::Cyan))?;
        }
        let line = clean(line, width - 4);
        write!(out, "{line}{}", " ".repeat(width - 4 - line.width()))?;
        if colors() {
            queue!(out, ResetColor)?;
        }
        writeln!(out, " │")?;
    }
    writeln!(out, "╰{}╯", "─".repeat(width - 2))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_providers::InstalledCli;

    fn settings() -> Settings {
        Settings {
            schema_version: 1,
            client: Backend::Codex,
            provider: "openai".into(),
            model: "test-model".into(),
            effort: "medium".into(),
            executable: "/test/bin/custom-wrapper".into(),
            providers_root: "/test/providers".into(),
            default_profile: Some("my-team".into()),
            max_agents: 4,
            ai_memory: true,
            ai_usagebar: false,
        }
    }

    fn inventory() -> Inventory {
        Inventory {
            installed: [
                ("codex", "Codex CLI", "/test/bin/codex", true),
                ("gemini", "Gemini CLI", "/test/bin/gemini", false),
                (
                    "configured",
                    "Wrapper configurado",
                    "/test/bin/custom-wrapper",
                    true,
                ),
            ]
            .into_iter()
            .map(|(id, name, executable, adapter_available)| InstalledCli {
                id: id.into(),
                name: name.into(),
                executable: executable.into(),
                adapter_available,
                other_installations: Vec::new(),
                note: None,
            })
            .collect(),
            searched_directories: Vec::new(),
            catalog: Vec::new(),
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn unicode_input_edits_at_character_boundaries_and_scrolls_to_the_caret() {
        let mut input = Input::new("aç界".into());
        input.key(key(KeyCode::Left));
        input.key(key(KeyCode::Backspace));
        assert_eq!(input.value, "a界");
        input.insert("🙂");
        assert_eq!(input.value, "a🙂界");
        input.key(key(KeyCode::Delete));
        assert_eq!(input.value, "a🙂");
        input.key(key(KeyCode::Home));
        input.key(key(KeyCode::Right));
        input.key(key(KeyCode::Delete));
        assert_eq!(input.value, "a");
        input.insert("ç\n界\t");
        assert_eq!(input.value, "aç界");
        for width in [1, 2, 3, 8] {
            let (visible, caret) = input.visible(width);
            assert!(visible.width() <= width);
            assert!(caret < width);
            assert!(!visible.chars().any(char::is_control));
        }
        input.key(key(KeyCode::Home));
        assert_eq!(input.visible(3), ("aç".into(), 0));
        input.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(input.value.is_empty());
        assert_eq!(input.visible(3), (String::new(), 0));
    }

    #[test]
    fn cli_without_adapter_cannot_advance_and_keyboard_selection_can_recover() {
        let original = settings();
        let inventory = inventory();
        let mut wizard =
            Wizard::new(&original, &inventory, Path::new("/test/config.json")).unwrap();
        assert_eq!(wizard.selected, 2);
        wizard.key(key(KeyCode::Up)).unwrap();
        assert!(wizard.key(key(KeyCode::Enter)).is_err());
        assert_eq!(wizard.step, 0);
        assert!(wizard.settings().is_err());
        wizard.key(key(KeyCode::Up)).unwrap();
        assert!(wizard.key(key(KeyCode::Enter)).unwrap().is_none());
        assert_eq!(wizard.step, 1);
        assert_eq!(
            wizard.settings().unwrap().executable,
            PathBuf::from("/test/bin/codex")
        );
    }

    #[test]
    fn confirming_a_new_client_changes_defaults_and_restores_each_client_draft() {
        let original = settings();
        let mut inventory = inventory();
        inventory.installed.push(InstalledCli {
            id: "claude".into(),
            name: "Claude Code".into(),
            executable: "/test/bin/claude".into(),
            adapter_available: true,
            other_installations: Vec::new(),
            note: None,
        });
        let mut wizard =
            Wizard::new(&original, &inventory, Path::new("/test/config.json")).unwrap();
        wizard.advance().unwrap();
        wizard.fields[1] = Input::new("edited-codex-model".into());
        wizard.key(key(KeyCode::Esc)).unwrap();
        wizard.key(key(KeyCode::Down)).unwrap();
        assert_eq!(wizard.active_client, Backend::Codex);
        assert_eq!(wizard.fields[1].value, "edited-codex-model");
        wizard.advance().unwrap();
        let claude = wizard.settings().unwrap();
        assert_eq!(claude.client, Backend::Claude);
        assert_eq!(claude.provider, "anthropic");
        assert_eq!(claude.model, "default");
        assert_eq!(claude.effort, "default");
        assert_eq!(claude.executable, PathBuf::from("/test/bin/claude"));
        assert_eq!(claude.providers_root, original.providers_root);
        wizard.fields[1] = Input::new("sonnet".into());
        wizard.key(key(KeyCode::Esc)).unwrap();
        wizard.key(key(KeyCode::Up)).unwrap();
        wizard.advance().unwrap();
        assert_eq!(wizard.active_client, Backend::Codex);
        assert_eq!(wizard.fields[1].value, "edited-codex-model");
        assert_eq!(wizard.settings().unwrap().executable, original.executable);
        wizard.key(key(KeyCode::Esc)).unwrap();
        wizard.key(key(KeyCode::Down)).unwrap();
        wizard.advance().unwrap();
        assert_eq!(wizard.fields[1].value, "sonnet");
        assert_eq!(wizard.settings().unwrap().client, Backend::Claude);
    }

    #[test]
    fn configured_wrapper_inherits_its_saved_client() {
        let mut original = settings();
        original.client = Backend::Cursor;
        original.provider = Backend::Cursor.default_provider().into();
        original.model = "default".into();
        original.effort = "default".into();
        let inventory = inventory();
        let mut wizard =
            Wizard::new(&original, &inventory, Path::new("/test/config.json")).unwrap();
        assert_eq!(wizard.selected, 2);
        wizard.advance().unwrap();
        let saved = wizard.settings().unwrap();
        assert_eq!(saved.client, Backend::Cursor);
        assert_eq!(saved.executable, original.executable);
    }

    #[test]
    fn draft_survives_back_navigation_and_returns_the_wrapper_only_after_confirmation() {
        let original = settings();
        let inventory = inventory();
        let mut wizard =
            Wizard::new(&original, &inventory, Path::new("/test/config.json")).unwrap();
        assert!(wizard.advance().unwrap().is_none());
        for _ in 0..3 {
            assert!(wizard.advance().unwrap().is_none());
        }
        assert_eq!(wizard.step, 2);
        wizard.fields[3] = Input::new(" profiles ".into());
        wizard.fields[5] = Input::new("invalid".into());
        wizard.key(key(KeyCode::Esc)).unwrap();
        assert_eq!(wizard.step, 1);
        for _ in 0..3 {
            assert!(wizard.advance().unwrap().is_none());
        }
        assert_eq!(wizard.step, 2);
        wizard.fields[5] = Input::new("8".into());
        for _ in 0..3 {
            assert!(wizard.advance().unwrap().is_none());
        }
        assert_eq!(wizard.step, 3);
        assert!(wizard.advance().unwrap().is_none());
        assert_eq!(wizard.step, 4);
        let saved = wizard.advance().unwrap().unwrap();
        assert_eq!(saved.executable, original.executable);
        assert_eq!(saved.providers_root, wizard.cwd.join("profiles"));
        assert_eq!(saved.max_agents, 8);
        assert_eq!(saved.default_profile, original.default_profile);
        assert_eq!(original.max_agents, 4);
        assert_eq!(original.providers_root, PathBuf::from("/test/providers"));
    }

    #[test]
    fn invalid_assistant_and_project_values_keep_the_user_on_the_current_step() {
        let original = settings();
        let inventory = inventory();
        let mut wizard =
            Wizard::new(&original, &inventory, Path::new("/test/config.json")).unwrap();
        wizard.step = 1;
        wizard.field = 2;
        wizard.fields[2] = Input::new("unrecognized-effort".into());
        assert!(wizard.advance().is_err());
        assert_eq!((wizard.step, wizard.field), (1, 2));
        wizard.fields[2] = Input::new(" high ".into());
        assert!(wizard.advance().unwrap().is_none());
        assert_eq!(wizard.step, 2);
        wizard.field = 2;
        for invalid in ["0", "101", "many"] {
            wizard.fields[5] = Input::new(invalid.into());
            assert!(wizard.advance().is_err());
            assert_eq!((wizard.step, wizard.field), (2, 2));
        }
        wizard.fields[5] = Input::new("4".into());
        wizard.fields[3] = Input::new("  ".into());
        assert!(wizard.advance().is_err());
        assert_eq!((wizard.step, wizard.field), (2, 2));
        wizard.fields[3] = Input::new("profiles".into());
        assert!(wizard.advance().unwrap().is_none());
        assert_eq!(wizard.step, 3);
    }

    #[test]
    fn continue_shortcut_validates_each_step_and_only_saves_after_review() {
        let original = settings();
        let inventory = inventory();
        let mut wizard =
            Wizard::new(&original, &inventory, Path::new("/test/config.json")).unwrap();
        assert!(wizard.key(key(KeyCode::F(5))).unwrap().is_none());
        assert_eq!((wizard.step, wizard.field), (1, 0));
        wizard.fields[1] = Input::new(String::new());
        assert!(wizard.key(key(KeyCode::F(5))).is_err());
        assert_eq!((wizard.step, wizard.field), (1, 0));
        wizard.fields[1] = Input::new("test-model".into());
        assert!(wizard.key(key(KeyCode::F(5))).unwrap().is_none());
        assert_eq!((wizard.step, wizard.field), (2, 0));
        wizard.fields[5] = Input::new("0".into());
        assert!(wizard.key(key(KeyCode::F(5))).is_err());
        assert_eq!((wizard.step, wizard.field), (2, 0));
        wizard.fields[5] = Input::new("7".into());
        assert!(wizard.key(key(KeyCode::F(5))).unwrap().is_none());
        assert_eq!(wizard.step, 3);
        wizard.key(key(KeyCode::Char(' '))).unwrap();
        assert!(wizard.key(key(KeyCode::F(2))).unwrap().is_none());
        assert_eq!((wizard.step, wizard.field), (2, 0));
        assert_eq!(wizard.fields[5].value, "7");
        assert!(wizard.key(key(KeyCode::F(5))).unwrap().is_none());
        assert!(!wizard.ai_memory);
        assert!(wizard.key(key(KeyCode::F(5))).unwrap().is_none());
        assert_eq!(wizard.step, 4);
        let saved = wizard.key(key(KeyCode::F(5))).unwrap().unwrap();
        assert_eq!(saved.max_agents, 7);
        assert!(!saved.ai_memory);
        assert_eq!(original.max_agents, 4);
    }

    #[test]
    fn memory_checkbox_defaults_to_selected_and_survives_back_navigation() {
        let original = settings();
        let inventory = inventory();
        let mut wizard =
            Wizard::new(&original, &inventory, Path::new("/test/config.json")).unwrap();
        wizard.step = 3;
        assert!(wizard.ai_memory);
        let frame = wizard.frame(56, 24);
        assert!(
            frame
                .spans
                .iter()
                .any(|span| span.text == "[x] Instalar AI-Memory")
        );
        wizard.key(key(KeyCode::Char(' '))).unwrap();
        assert!(!wizard.ai_memory);
        assert!(wizard.key(key(KeyCode::Enter)).unwrap().is_none());
        assert_eq!(wizard.step, 4);
        wizard.key(key(KeyCode::Esc)).unwrap();
        assert_eq!(wizard.step, 3);
        assert!(!wizard.ai_memory);
        assert!(wizard.key(key(KeyCode::Enter)).unwrap().is_none());
        let saved = wizard.key(key(KeyCode::Enter)).unwrap().unwrap();
        assert!(!saved.ai_memory);
        assert!(original.ai_memory);
    }

    #[test]
    fn reopening_a_deselected_memory_setting_and_cancelling_preserves_saved_choice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut original = settings();
        original.ai_memory = false;
        original.save(&path).unwrap();
        let saved = Settings::read(&path).unwrap();
        let inventory = inventory();
        {
            let mut wizard = Wizard::new(&saved, &inventory, &path).unwrap();
            wizard.step = 3;
            assert!(!wizard.ai_memory);
            wizard.key(key(KeyCode::Char(' '))).unwrap();
            assert!(wizard.ai_memory);
            for _ in 0..3 {
                assert!(wizard.key(key(KeyCode::Esc)).unwrap().is_none());
            }
            assert_eq!(wizard.step, 0);
            // Cancellation drops the draft; only the caller persists a confirmed result.
        }
        assert!(!Settings::read(&path).unwrap().ai_memory);
    }

    #[test]
    fn supported_usagebar_starts_unchecked_and_saves_independent_checkbox_choice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let original = settings();
        let inventory = inventory();
        let mut wizard =
            Wizard::new_with_usagebar_support(&original, &inventory, &path, true).unwrap();
        wizard.step = 3;
        assert!(wizard.ai_memory);
        assert!(!wizard.ai_usagebar);
        assert!(
            wizard
                .frame(56, 24)
                .spans
                .iter()
                .any(|span| { span.text == "[ ] Instalar AI-UsageBar" })
        );
        wizard.key(key(KeyCode::Down)).unwrap();
        assert_eq!(wizard.optional_selected, 1);
        wizard.key(key(KeyCode::Char(' '))).unwrap();
        assert!(wizard.ai_memory && wizard.ai_usagebar);
        wizard.key(key(KeyCode::Tab)).unwrap();
        assert_eq!(wizard.optional_selected, 0);
        wizard.key(key(KeyCode::Char(' '))).unwrap();
        assert!(!wizard.ai_memory && wizard.ai_usagebar);
        wizard.key(key(KeyCode::BackTab)).unwrap();
        assert_eq!(wizard.optional_selected, 1);
        wizard.key(key(KeyCode::F(5))).unwrap();
        assert_eq!(wizard.step, 4);
        assert!(
            wizard
                .frame(56, 24)
                .spans
                .iter()
                .any(|span| { span.text == "[x] Instalar AI-UsageBar" })
        );
        wizard.key(key(KeyCode::F(2))).unwrap();
        assert_eq!(wizard.step, 3);
        assert!(!wizard.ai_memory && wizard.ai_usagebar);
        wizard.key(key(KeyCode::Enter)).unwrap();
        let saved = wizard.key(key(KeyCode::Enter)).unwrap().unwrap();
        saved.save(&path).unwrap();
        let saved = Settings::read(&path).unwrap();
        assert!(!saved.ai_memory && saved.ai_usagebar);
        let reopened = Wizard::new_with_usagebar_support(&saved, &inventory, &path, true).unwrap();
        assert!(!reopened.ai_memory && reopened.ai_usagebar);
    }

    #[test]
    fn unsupported_usagebar_is_hidden_and_cannot_be_selected_from_existing_configuration() {
        let mut original = settings();
        original.ai_usagebar = true;
        let inventory = inventory();
        let mut wizard = Wizard::new_with_usagebar_support(
            &original,
            &inventory,
            Path::new("/test/config.json"),
            false,
        )
        .unwrap();
        assert!(!wizard.ai_usagebar);
        wizard.step = 3;
        for code in [KeyCode::Down, KeyCode::Tab, KeyCode::Up, KeyCode::BackTab] {
            wizard.key(key(code)).unwrap();
            assert_eq!(wizard.optional_selected, 0);
        }
        wizard.key(key(KeyCode::Char(' '))).unwrap();
        assert!(!wizard.ai_memory && !wizard.ai_usagebar);
        for step in 3..=4 {
            wizard.step = step;
            assert!(!wizard.frame(56, 24).spans.iter().any(|span| {
                span.text.contains("UsageBar") || span.text.contains("ai-usagebar")
            }));
        }
        let draft = wizard.key(key(KeyCode::Enter)).unwrap().unwrap();
        assert!(!draft.ai_usagebar);
        assert!(original.ai_usagebar);
    }

    #[test]
    fn usagebar_description_and_credits_fit_the_minimum_setup_terminal() {
        let original = settings();
        let inventory = inventory();
        let mut wizard = Wizard::new_with_usagebar_support(
            &original,
            &inventory,
            Path::new("/test/config.json"),
            true,
        )
        .unwrap();
        wizard.step = 3;
        wizard.key(key(KeyCode::Down)).unwrap();
        let frame = wizard.frame(56, 24);
        for text in [
            "[x] Instalar AI-Memory",
            "[ ] Instalar AI-UsageBar",
            "AI-UsageBar · consumo dos CLIs no terminal",
            "Barra gráfica: configure separadamente.",
            "Criado por Fabio Akita (AkitaOnRails)",
            crate::usagebar::PROJECT_URL,
            crate::memory::CREATOR_URL,
        ] {
            assert!(frame.spans.iter().any(|span| span.text == text), "{text}");
        }
        for span in frame.spans {
            assert!(span.y < 24);
            assert!(span.x + span.text.width() <= 56);
        }
    }

    #[test]
    fn setup_hierarchy_and_review_adapt_to_available_width() {
        let original = settings();
        let inventory = inventory();
        let mut wizard = Wizard::new_with_usagebar_support(
            &original,
            &inventory,
            Path::new("/test/config.json"),
            true,
        )
        .unwrap();

        let first = wizard.frame(56, 24);
        assert!(
            first
                .spans
                .iter()
                .any(|span| span.text == "ETAPA 1/5 · 20%")
        );
        assert!(
            first
                .spans
                .iter()
                .any(|span| span.text.contains("1 CLI") || span.text.contains("3 CLIs"))
        );

        wizard.step = 1;
        let fields = wizard.frame(94, 30);
        assert!(
            fields
                .spans
                .iter()
                .any(|span| span.text.contains("campo 1 de 3"))
        );
        assert!(fields.spans.iter().any(|span| span.text == "EDITANDO"));

        wizard.step = 4;
        let compact = wizard.frame(56, 24);
        assert!(
            compact
                .spans
                .iter()
                .any(|span| span.text.starts_with("AUXILIAR "))
        );
        assert!(
            compact
                .spans
                .iter()
                .any(|span| span.text == "[ ] Instalar AI-UsageBar")
        );
        let wide = wizard.frame(94, 30);
        assert!(
            wide.spans
                .iter()
                .any(|span| span.x > 3 && span.text.starts_with("PROJETO E EQUIPE"))
        );
        assert!(
            wide.spans
                .iter()
                .any(|span| span.text.starts_with("COMPLEMENTOS"))
        );
    }

    #[test]
    fn frames_and_carets_stay_within_small_and_large_terminal_bounds() {
        let original = settings();
        let mut inventory = inventory();
        for i in 0..12 {
            inventory.installed.push(InstalledCli {
                id: format!("fixture-{i}"),
                name: "Nome com acentos e 界 muito longo para a coluna".into(),
                executable: format!("/a/very/long/path/to/provider/{i}").into(),
                other_installations: Vec::new(),
                adapter_available: false,
                note: Some("Nota longa\ncom controles\tpara limpar".into()),
            });
        }
        let mut wizard =
            Wizard::new(&original, &inventory, Path::new("/test/config.json")).unwrap();
        wizard.fields[1] = Input::new("modelo-ç界".repeat(12));
        wizard.fields[3] = Input::new(format!("/profiles/{}", "pasta-ç界".repeat(30)));
        wizard.error = Some("Erro de validação\nque precisa caber no painel".into());
        for (width, height) in [(56, 24), (94, 30), (110, 30)] {
            for step in 0..5 {
                wizard.step = step;
                wizard.selected = if step == 0 {
                    inventory.installed.len() - 1
                } else {
                    2
                };
                if step >= 3 {
                    assert!(wizard.settings().is_ok());
                }
                for field in 0..3 {
                    wizard.field = field;
                    let frame = wizard.frame(width, height);
                    for span in &frame.spans {
                        assert!(span.y < height);
                        assert!(span.x + span.text.width() <= width);
                        assert!(!span.text.chars().any(char::is_control));
                    }
                    if let Some((x, y)) = frame.caret {
                        assert!(x > 0 && x < width - 1);
                        assert!(y > 0 && y < height - 1);
                    } else {
                        assert!(step == 0 || step >= 3);
                    }
                }
            }
        }
    }
}
