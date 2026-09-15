use ai_token_timeline::{
    analytics::{self, Period, Scope},
    app_ui, chat_ui,
    cli_providers::{self, InstalledCli},
    client::Backend,
    collector,
    db::Db,
    memory,
    model::{Annotation, Dataset, Price},
    profiles::{self, Settings},
    setup_flow, setup_ui, trend,
    ui_data::Page,
    usagebar, widget, workflow,
    workspace::Workspace,
};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "stackpulse",
    bin_name = "stackpulse",
    version,
    about = "Widget de tokens, custo e tempo de agentes no terminal"
)]
struct Cli {
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Autoriza nesta sessão a pasta atual exata; necessário em scripts sem terminal"
    )]
    allow_workspace: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        help = "SQLite local (padrão: ~/.local/share/ai-token-timeline/usage.sqlite)"
    )]
    db: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        help = "Setup do auxiliar (padrão: ~/.config/ai-token-timeline/config.json)"
    )]
    config: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        help = "Pasta de JSONL do Codex (padrão: $CODEX_HOME/sessions ou ~/.codex/sessions)"
    )]
    sessions: Option<PathBuf>,
    #[arg(long, global = true, default_value = "America/Sao_Paulo")]
    timezone: Tz,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Args, Clone, Default)]
struct Filter {
    #[arg(long, help = "Diretório do projeto (inclui os subagentes da execução)")]
    project: Option<PathBuf>,
    #[arg(long, help = "ID ou prefixo único da execução raiz")]
    run: Option<String>,
}
impl Filter {
    fn scope(&self, db: &Db) -> Result<Scope> {
        Ok(Scope {
            project: self.project.as_ref().map(|p| {
                p.canonicalize()
                    .unwrap_or_else(|_| p.clone())
                    .to_string_lossy()
                    .into()
            }),
            run: self
                .run
                .as_deref()
                .map(|r| analytics::resolve_run(&db.snapshot()?, r))
                .transpose()?,
        })
    }
}

#[derive(Args)]
struct Widget {
    #[arg(long, help = "Inicia na visão diária de feedback e softline")]
    quality: bool,
    #[arg(long, value_enum, default_value = "day")]
    period: Period,
    #[arg(long, help = "Imprime uma vez, sem interação")]
    once: bool,
    #[arg(long, help = "Uma linha, adequada para tmux/status bar")]
    line: bool,
    #[arg(long, help = "Lê somente o banco; não coleta JSONL")]
    no_sync: bool,
    #[arg(long,default_value_t=5,value_parser=clap::value_parser!(u64).range(1..3601))]
    interval: u64,
    #[command(flatten)]
    filter: Filter,
}
impl Default for Widget {
    fn default() -> Self {
        Self {
            quality: false,
            period: Period::Day,
            once: false,
            line: false,
            no_sync: false,
            interval: 5,
            filter: Filter::default(),
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Escolhe um perfil e abre o terminal de pedidos; aceita telas de gestão.
    Ui {
        #[arg(value_enum)]
        page: Option<Page>,
    },
    /// Configura o auxiliar que interpreta imagens e a pasta de perfis.
    Setup(Setup),
    /// Instala a memória opcional AI-Memory, criada por Fabio Akita.
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
    },
    /// Instala o CLI/TUI opcional AI-UsageBar, criado por Fabio Akita.
    Usagebar {
        #[command(subcommand)]
        command: UsagebarCommand,
    },
    /// Seleção conjunta usada pelo instalador do StackPulse.
    #[command(hide = true)]
    Plugins {
        #[command(subcommand)]
        command: PluginsCommand,
    },
    /// Descobre e compila perfis de equipe a partir de imagens.
    Profiles {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    /// Executa um pedido usando o perfil da pasta e salva stack, consumo e tempo.
    Run {
        prompt: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long, default_value = "")]
        benchmark: String,
        #[arg(long,value_parser=["read-only","workspace-write"],default_value="workspace-write")]
        sandbox: String,
        #[arg(long,default_value_t=1800,value_parser=clap::value_parser!(u64).range(1..86401))]
        timeout: u64,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        no_feedback: bool,
    },
    /// Compara a entrega, tempo e tokens de dois ou mais perfis em paralelo.
    Compare {
        #[arg(
            long = "profile",
            help = "Perfil participante; repita para cada perfil"
        )]
        profiles: Vec<String>,
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long, help = "Comando de teste executado em cada entrega isolada")]
        test: Option<String>,
        #[arg(
            long,
            help = "Autoriza criar e manter os arquivos da comparação na pasta temporária"
        )]
        allow_tmp: bool,
        #[arg(long, default_value_t = 1800, value_parser=clap::value_parser!(u64).range(1..86401))]
        timeout: u64,
    },
    /// Lista registros das execuções iniciadas pelo StackPulse.
    Executions {
        #[arg(long)]
        json: bool,
    },
    /// Registra entrega (0..1) e rapidez percebida (1..5; 0 não avaliada).
    Feedback {
        execution: String,
        #[arg(long)]
        delivered: f64,
        #[arg(long)]
        speed: u8,
        #[arg(long, default_value = "")]
        note: String,
    },
    /// Linhas diárias de entrega/rapidez com suavização EWMA.
    Trend {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        benchmark: Option<String>,
        #[arg(long,default_value_t=30,value_parser=clap::value_parser!(u32).range(2..367))]
        days: u32,
        #[arg(long, default_value_t = 0.3)]
        alpha: f64,
        #[arg(long)]
        json: bool,
    },
    /// Widget compacto com h/d/m e q para sair.
    Widget(Widget),
    /// Importa sessões locais, sem duplicar o histórico já salvo.
    Sync,
    /// Lista execuções e a composição real dos agentes.
    Runs {
        #[command(flatten)]
        filter: Filter,
        #[arg(long)]
        json: bool,
    },
    /// Exporta as métricas da janela em JSON.
    Report {
        #[command(flatten)]
        filter: Filter,
        #[arg(long, value_enum, default_value = "day")]
        period: Period,
    },
    /// Anota um benchmark repetível e a qualidade entre 0 e 1.
    Tag {
        run: String,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        benchmark: Option<String>,
        #[arg(long)]
        quality: Option<f64>,
        #[arg(long, conflicts_with = "current")]
        baseline: bool,
        #[arg(long)]
        current: bool,
    },
    /// Compara baseline e execuções atuais de benchmark/configuração idênticos.
    BenchmarkCompare {
        #[arg(long)]
        json: bool,
    },
    /// Cadastra tarifa versionada; todos os valores são USD por milhão de tokens.
    Price {
        #[arg(long)]
        provider: String,
        #[arg(long)]
        model: String,
        #[arg(long, default_value = "unknown")]
        service_tier: String,
        #[arg(long)]
        effective_at: DateTime<Utc>,
        #[arg(long)]
        input: f64,
        #[arg(long)]
        cached: f64,
        #[arg(long)]
        cache_write: f64,
        #[arg(long)]
        output: f64,
        #[arg(long)]
        source: String,
    },
    /// Importa métricas normalizadas de outros provedores (JSON documentado).
    Import { file: PathBuf },
}

#[derive(Args)]
struct Setup {
    #[arg(
        long,
        conflicts_with = "list_clis",
        help = "Usa perguntas em texto, sem a interface visual"
    )]
    plain: bool,
    #[arg(long, conflicts_with_all = ["client", "provider", "model", "effort", "providers_root", "default_profile", "executable", "max_agents", "ai_memory", "ai_usagebar"],
        help = "Lista CLIs instalados sem alterar o setup")]
    list_clis: bool,
    #[arg(long, requires = "list_clis", help = "Inventário de CLIs em JSON")]
    json: bool,
    #[arg(
        long,
        value_enum,
        help = "CLI usado pelo auxiliar; reutiliza a autenticação existente"
    )]
    client: Option<Backend>,
    #[arg(long)]
    provider: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    effort: Option<String>,
    #[arg(long)]
    providers_root: Option<PathBuf>,
    #[arg(long)]
    default_profile: Option<String>,
    #[arg(
        long,
        help = "Caminho ou comando do CLI escolhido (ou wrapper com a mesma interface)"
    )]
    executable: Option<PathBuf>,
    #[arg(long)]
    max_agents: Option<u32>,
    #[arg(
        long,
        value_name = "true|false",
        help = "Seleciona a instalação do AI-Memory; true instala o pacote oficial"
    )]
    ai_memory: Option<bool>,
    #[arg(
        long,
        value_name = "true|false",
        help = "Seleciona o AI-UsageBar (padrão: false); true instala CLI e TUI em plataforma compatível"
    )]
    ai_usagebar: Option<bool>,
}

#[derive(Subcommand)]
enum UsagebarCommand {
    /// Verifica compatibilidade sem instalar: saída 0 compatível, 1 indisponível.
    Supported,
    /// Instala CLI e TUI oficiais; barras do desktop são configuradas separadamente.
    Install {
        #[arg(long, help = "Prefixo local de instalação (padrão: ~/.local)")]
        prefix: Option<PathBuf>,
        #[arg(
            long,
            help = "Mostra checkbox desmarcada por padrão; Espaço alterna, Enter confirma"
        )]
        select: bool,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum PluginMode {
    Select,
    With,
    Without,
}
impl PluginMode {
    fn choice(self) -> Option<bool> {
        match self {
            Self::Select => None,
            Self::With => Some(true),
            Self::Without => Some(false),
        }
    }
}

#[derive(Subcommand)]
enum PluginsCommand {
    Select {
        #[arg(long, value_enum, default_value = "select")]
        memory: PluginMode,
        #[arg(long, value_enum, default_value = "select")]
        usagebar: PluginMode,
        #[arg(long)]
        result_file: PathBuf,
    },
}

#[derive(Subcommand)]
enum MemoryCommand {
    /// Instala o pacote oficial; conexão aos agentes é uma etapa separada.
    Install {
        #[arg(long, help = "Prefixo local de instalação (padrão: ~/.local)")]
        prefix: Option<PathBuf>,
        #[arg(
            long,
            help = "Mostra checkbox marcada por padrão; Espaço alterna, Enter confirma"
        )]
        select: bool,
    },
}

#[derive(Subcommand)]
enum ProfileCommand {
    List {
        #[arg(long)]
        json: bool,
    },
    Compile {
        image: PathBuf,
        #[arg(long)]
        force: bool,
        #[arg(long,default_value_t=300,value_parser=clap::value_parser!(u64).range(1..3601))]
        timeout: u64,
    },
    Add {
        image: PathBuf,
        #[arg(long, default_value = "all")]
        scope: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        no_compile: bool,
    },
}

fn setup(args: Setup, path: &std::path::Path, cwd: &std::path::Path) -> Result<()> {
    use std::io::IsTerminal;
    let dirs = cli_providers::search_directories();
    let mut inventory = cli_providers::discover(&dirs);
    if args.list_clis {
        if args.json {
            println!("{}", serde_json::to_string_pretty(&inventory)?);
        } else {
            cli_providers::print_inventory(&inventory);
        }
        return Ok(());
    }
    let interactive = args.client.is_none()
        && args.provider.is_none()
        && args.model.is_none()
        && args.effort.is_none()
        && args.providers_root.is_none()
        && args.default_profile.is_none()
        && args.executable.is_none()
        && args.max_agents.is_none()
        && args.ai_memory.is_none()
        && args.ai_usagebar.is_none();
    // Unrelated scripted configuration edits must not install software implicitly.
    let install_memory = interactive || args.ai_memory == Some(true);
    let install_usagebar = interactive || args.ai_usagebar == Some(true);
    if args.ai_usagebar == Some(true) {
        ensure!(usagebar::supported(), usagebar::platform_description());
    }
    let mut settings = if path.exists() {
        Settings::read(path)?
    } else {
        Settings {
            schema_version: 1,
            client: Backend::Codex,
            provider: "openai".into(),
            model: "gpt-6-astra".into(),
            effort: "medium".into(),
            executable: "codex".into(),
            providers_root: cwd.join("providers"),
            default_profile: None,
            max_agents: 4,
            ai_memory: true,
            ai_usagebar: false,
        }
    };
    let requested_executable = args
        .executable
        .as_ref()
        .and_then(|path| cli_providers::resolve_executable(path, &dirs));
    let detected_client = requested_executable.as_ref().and_then(|path| {
        inventory
            .installed
            .iter()
            .find(|cli| cli_has_executable(cli, path))
            .and_then(|cli| Backend::from_cli_id(&cli.id))
    });
    if let (Some(requested), Some(detected)) = (args.client, detected_client) {
        ensure!(
            requested == detected,
            "--client {} não corresponde ao executável {}.",
            requested.id(),
            detected.label()
        );
    }
    if let Some(client) = args.client.or(detected_client) {
        switch_client(&mut settings, client);
        if args.executable.is_none() {
            settings.executable = inventory
                .installed
                .iter()
                .find(|cli| cli.id == client.id())
                .map(|cli| cli.executable.clone())
                .unwrap_or_else(|| client.command().into());
        }
    }
    let configured = args.executable.as_ref().unwrap_or(&settings.executable);
    let resolved = cli_providers::resolve_executable(configured, &dirs);
    if let Some(executable) = &resolved
        && !inventory
            .installed
            .iter()
            .any(|cli| cli_has_executable(cli, executable))
    {
        inventory.installed.push(InstalledCli {
            id: "configured".into(),
            name: format!("Executável configurado ({})", settings.client.label()),
            executable: executable.clone(),
            other_installations: Vec::new(),
            adapter_available: true,
            note: Some("Wrapper informado no setup; compatibilidade não verificada.".into()),
        });
    }
    let visual = interactive && !args.plain && setup_ui::available();
    if !visual {
        cli_providers::print_inventory(&inventory);
    }
    ensure!(
        !interactive || std::io::stdin().is_terminal(),
        "Use setup --list-clis para consultar ou setup --provider openai --model MODELO para configurar sem interação"
    );
    if visual {
        if let Some(executable) = &resolved {
            settings.executable = executable.clone();
        }
        loop {
            let Some(draft) = setup_ui::run(&settings, &inventory, path)? else {
                return Ok(());
            };
            settings = draft;
            match setup_flow::finish_setup(&settings, path, cwd)? {
                setup_flow::Outcome::Done => return Ok(()),
                setup_flow::Outcome::Review => continue,
                setup_flow::Outcome::Failed => {
                    anyhow::bail!("Setup encerrado com uma etapa pendente.")
                }
            }
        }
    } else if interactive {
        let default = inventory
            .installed
            .iter()
            .position(|cli| {
                cli.adapter_available
                    && resolved
                        .as_ref()
                        .is_some_and(|p| cli_has_executable(cli, p))
            })
            .or_else(|| {
                inventory
                    .installed
                    .iter()
                    .position(|cli| cli.adapter_available)
            });
        let default = default.context(
            "Nenhum CLI com adaptador disponível. Instale um CLI suportado ou configure --client e --executable."
        )?;
        println!("Escolha um CLI com integração disponível para o auxiliar.");
        loop {
            let answer = workflow::ask("Número do CLI", &(default + 1).to_string())?;
            let selected = answer
                .parse::<usize>()
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|n| inventory.installed.get(n));
            match selected {
                Some(cli) if cli.adapter_available => {
                    if let Some(client) = Backend::from_cli_id(&cli.id) {
                        switch_client(&mut settings, client);
                    }
                    settings.executable = resolved
                        .as_ref()
                        .filter(|path| cli_has_executable(cli, path))
                        .unwrap_or(&cli.executable)
                        .clone();
                    break;
                }
                Some(cli) => println!(
                    "{} está instalado, mas o StackPulse ainda não possui adaptador para ele.",
                    cli.name
                ),
                None => println!("Escolha um número da lista."),
            }
        }
        println!(
            "{} · login existente · default usa a configuração do CLI.",
            settings.client.label()
        );
        settings.provider = workflow::ask("Provider do auxiliar", &settings.provider)?;
        settings.model = workflow::ask("Modelo com visão (ou default)", &settings.model)?;
        settings.effort = workflow::ask("Esforço (ou default)", &settings.effort)?;
        settings.providers_root = PathBuf::from(workflow::ask(
            "Pasta providers",
            &settings.providers_root.to_string_lossy(),
        )?);
        let default = workflow::ask(
            "Perfil padrão (vazio escolhe automaticamente quando houver só um)",
            settings.default_profile.as_deref().unwrap_or(""),
        )?;
        settings.default_profile = (!default.is_empty()).then_some(default);
        settings.max_agents = workflow::ask(
            "Máximo de subagentes simultâneos",
            &settings.max_agents.to_string(),
        )?
        .parse()?;
        memory::print_credits();
        println!("Instala o pacote oficial. Conexão aos agentes é uma etapa separada.");
        settings.ai_memory = memory::ask_install(settings.ai_memory)?;
        settings.ai_usagebar = if usagebar::supported() {
            usagebar::print_credits();
            println!("{}", usagebar::platform_description());
            usagebar::ask_install(settings.ai_usagebar)?
        } else {
            false
        };
    } else {
        if let Some(v) = args.provider {
            settings.provider = v;
        }
        if let Some(v) = args.model {
            settings.model = v;
        }
        if let Some(v) = args.effort {
            settings.effort = v;
        }
        if let Some(v) = args.providers_root {
            settings.providers_root = v;
        }
        if let Some(v) = args.default_profile {
            settings.default_profile = Some(v);
        }
        if let Some(v) = args.executable {
            settings.executable = v;
        }
        if let Some(v) = args.max_agents {
            settings.max_agents = v;
        }
        if let Some(value) = args.ai_memory {
            settings.ai_memory = value;
        }
        if let Some(value) = args.ai_usagebar {
            settings.ai_usagebar = value;
        }
    }
    if settings.providers_root.is_relative() {
        settings.providers_root = cwd.join(&settings.providers_root);
    }
    if let Some(executable) = cli_providers::resolve_executable(&settings.executable, &dirs) {
        if let Some(cli) = inventory
            .installed
            .iter()
            .find(|cli| cli_has_executable(cli, &executable))
        {
            ensure!(
                cli.adapter_available,
                "{} está instalado, mas não possui adaptador no StackPulse.",
                cli.name
            );
            if let Some(client) = Backend::from_cli_id(&cli.id) {
                ensure!(
                    client == settings.client,
                    "CLI {} não corresponde ao client {} configurado.",
                    cli.name,
                    settings.client.label()
                );
            }
        }
        settings.executable = executable;
    } else {
        println!(
            "Executável {} não encontrado; instale-o antes de compilar ou executar perfis.",
            settings.executable.display()
        );
    }
    settings.validate()?;
    std::fs::create_dir_all(settings.providers_root.join("project/all"))?;
    settings.save(path)?;
    println!(
        "Setup salvo em {}\nCLI: {}\nAuxiliar: {} / {} / {}\nImagens: {}/project/{{all|projeto}}/equipe.png",
        path.display(),
        settings.executable.display(),
        settings.provider,
        settings.model,
        settings.effort,
        settings.providers_root.display()
    );
    let mut optional_errors = Vec::new();
    if settings.ai_memory && install_memory {
        memory::print_credits();
        match memory::install(&memory::default_prefix()?) {
            Ok(installed) => memory::print_installation(&installed),
            Err(error) => optional_errors.push(format!("AI-Memory: {error:#}")),
        }
    } else if settings.ai_memory {
        println!("AI-Memory selecionado na configuração. Instale com stackpulse memory install.");
    } else {
        println!("AI-Memory não selecionado. Instalações existentes foram preservadas.");
    }
    if usagebar::supported() {
        if settings.ai_usagebar && install_usagebar {
            match usagebar::install(&memory::default_prefix()?) {
                Ok(installed) => usagebar::print_installation(&installed),
                Err(error) => optional_errors.push(format!("AI-UsageBar: {error:#}")),
            }
        } else if settings.ai_usagebar {
            println!("AI-UsageBar selecionado. Instale com stackpulse usagebar install.");
        } else {
            println!("AI-UsageBar não selecionado.");
        }
    }
    ensure!(
        optional_errors.is_empty(),
        "Setup salvo. Instalação opcional pendente: {}",
        optional_errors.join("; ")
    );
    Ok(())
}

fn cli_has_executable(cli: &InstalledCli, executable: &std::path::Path) -> bool {
    std::iter::once(&cli.executable)
        .chain(&cli.other_installations)
        .any(|path| cli_providers::same_executable(path, executable))
}

fn switch_client(settings: &mut Settings, client: Backend) {
    if settings.client != client {
        settings.client = client;
        settings.provider = client.default_provider().into();
        settings.model = client.default_model().into();
        settings.effort = client.default_effort().into();
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let workspace = Workspace::current()?;
    if !workspace.authorize(cli.allow_workspace.as_deref())? {
        eprintln!("Acesso cancelado. Nenhum comando foi executado.");
        std::process::exit(1);
    }
    let cwd = workspace.path;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let db_path = cli
        .db
        .unwrap_or_else(|| home.join(".local/share/ai-token-timeline/usage.sqlite"));
    let config_path = cli
        .config
        .unwrap_or_else(|| home.join(".config/ai-token-timeline/config.json"));
    use std::io::IsTerminal;
    let command = match cli.command.unwrap_or_else(|| {
        if std::io::stdin().is_terminal()
            && std::io::stdout().is_terminal()
            && std::env::var("TERM").as_deref() != Ok("dumb")
        {
            Command::Ui { page: None }
        } else {
            Command::Widget(Widget::default())
        }
    }) {
        Command::Setup(args) => return setup(args, &config_path, &cwd),
        Command::Plugins {
            command:
                PluginsCommand::Select {
                    memory,
                    usagebar,
                    result_file,
                },
        } => {
            return ai_token_timeline::plugins::select_to_file(
                memory.choice(),
                usagebar.choice(),
                &result_file,
            );
        }
        Command::Usagebar {
            command: UsagebarCommand::Supported,
        } => {
            std::process::exit(if usagebar::supported() { 0 } else { 1 });
        }
        Command::Usagebar {
            command: UsagebarCommand::Install { prefix, select },
        } => {
            ensure!(usagebar::supported(), usagebar::platform_description());
            if select && !usagebar::select_install(false)? {
                println!("AI-UsageBar não selecionado.");
                std::process::exit(10);
            }
            let prefix = prefix.map(Ok).unwrap_or_else(memory::default_prefix)?;
            if setup_flow::available() {
                ensure!(
                    setup_flow::install_usagebar(&prefix, &cwd)? == setup_flow::Outcome::Done,
                    "Instalação do AI-UsageBar não concluída."
                );
                return Ok(());
            }
            let installed = usagebar::install(&prefix)?;
            usagebar::print_installation(&installed);
            return Ok(());
        }
        Command::Memory {
            command: MemoryCommand::Install { prefix, select },
        } => {
            if select && !memory::select_install(true)? {
                println!("AI-Memory não selecionado. Instalações existentes foram preservadas.");
                std::process::exit(10);
            }
            let prefix = prefix.map(Ok).unwrap_or_else(memory::default_prefix)?;
            if setup_flow::available() {
                ensure!(
                    setup_flow::install(&prefix, &cwd)? == setup_flow::Outcome::Done,
                    "Instalação do AI-Memory não concluída."
                );
                return Ok(());
            }
            if !select {
                memory::print_credits();
            }
            let installed = memory::install(&prefix)?;
            memory::print_installation(&installed);
            return Ok(());
        }
        command => command,
    };
    let sessions = cli.sessions.unwrap_or_else(|| {
        std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"))
            .join("sessions")
    });
    let mut db = Db::open(&db_path)?;
    match command {
        Command::Ui { page } => {
            let options = app_ui::Options {
                cwd: cwd.clone(),
                db: db_path,
                config: config_path,
                sessions,
                timezone: cli.timezone,
                page: page.unwrap_or(Page::Run),
            };
            if page.is_none() || page == Some(Page::Run) {
                chat_ui::run(&mut db, options)?;
            } else {
                app_ui::run(&db, options)?;
            }
        }
        Command::Setup(_)
        | Command::Memory { .. }
        | Command::Usagebar { .. }
        | Command::Plugins { .. } => {
            unreachable!("setup and optional packages are handled before opening the database")
        }
        Command::Profiles { command } => {
            let settings = Settings::read(&config_path)?;
            match command {
                ProfileCommand::List { json } => {
                    let list = profiles::discover(&settings, &cwd)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&list)?);
                    } else {
                        for p in list {
                            println!(
                                "{} · {} · {} · {}",
                                p.name,
                                p.scope,
                                if p.compiled {
                                    "pronto"
                                } else {
                                    "imagem pendente"
                                },
                                p.path.display()
                            );
                        }
                    }
                }
                ProfileCommand::Compile {
                    image,
                    force,
                    timeout,
                } => println!(
                    "Perfil salvo: {}",
                    workflow::compile(&settings, &image, timeout, force)?.display()
                ),
                ProfileCommand::Add {
                    image,
                    scope,
                    name,
                    no_compile,
                } => {
                    profiles::scope_name(&scope)?;
                    let name = name
                        .or_else(|| {
                            image
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .map(str::to_string)
                        })
                        .context("Imagem sem nome")?;
                    profiles::identifier(&name)?;
                    let ext = image
                        .extension()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    ensure!(
                        ["png", "jpg", "jpeg", "webp"].contains(&ext.as_str()),
                        "Formato de imagem não suportado"
                    );
                    let dest = settings
                        .providers_root
                        .join("project")
                        .join(scope)
                        .join(format!("{name}.{ext}"));
                    ensure!(!dest.exists(), "Imagem já existe: {}", dest.display());
                    profiles::atomic_write(&dest, &std::fs::read(image)?)?;
                    if no_compile {
                        println!("Imagem salva: {}", dest.display());
                    } else {
                        println!(
                            "Perfil salvo: {}",
                            workflow::compile(&settings, &dest, 300, false)?.display()
                        );
                    }
                }
            }
        }
        Command::Run {
            prompt,
            profile,
            benchmark,
            sandbox,
            timeout,
            dry_run,
            no_feedback,
        } => {
            let settings = Settings::read(&config_path)?;
            let selected = profiles::resolve(&settings, &cwd, profile.as_deref())?;
            if !selected.compiled && !dry_run {
                workflow::compile(&settings, &selected.path, 300, false)?;
            }
            if let Some(job) = workflow::run(
                &mut db,
                &settings,
                workflow::RunOptions {
                    cwd: &cwd,
                    sessions: &sessions,
                    profile: profile.as_deref(),
                    prompt: &prompt,
                    benchmark: &benchmark,
                    sandbox: &sandbox,
                    timeout_secs: timeout,
                    dry_run,
                    no_feedback,
                },
            )? {
                ensure!(
                    job.status == "completed",
                    "Execução {}: {}. O registro foi preservado",
                    job.id,
                    job.status
                );
            }
        }
        Command::Compare {
            profiles: selected,
            prompt,
            test,
            allow_tmp,
            timeout,
        } => {
            let settings = Settings::read(&config_path)?;
            let available = profiles::discover(&settings, &cwd)?;
            ensure!(
                available.len() >= 2,
                "A comparação exige ao menos 2 perfis. Crie novos perfis com stackpulse profiles add."
            );
            let selected = if selected.is_empty() {
                ai_token_timeline::compare_ui::select(&available)?
            } else {
                selected
            };
            let unique: std::collections::HashSet<_> = selected.iter().collect();
            ensure!(
                unique.len() >= 2 && unique.len() == selected.len(),
                "Selecione ao menos 2 perfis distintos, sem repetições."
            );
            for name in &selected {
                ensure!(
                    available.iter().any(|p| &p.name == name),
                    "Perfil {name} não encontrado"
                );
            }
            let (prompt, validation) = ai_token_timeline::compare_ui::request(prompt, test)?;
            ai_token_timeline::compare_ui::authorize_tmp(allow_tmp)?;
            println!(
                "Comparando {} perfis em execuções separadas…",
                selected.len()
            );
            let report = ai_token_timeline::compare::run(
                &mut db,
                &settings,
                ai_token_timeline::compare::CompareOptions {
                    cwd: &cwd,
                    sessions: &sessions,
                    profiles: &selected,
                    prompt: &prompt,
                    timeout_secs: timeout,
                    validation_command: validation.as_deref(),
                },
            )?;
            ai_token_timeline::compare_ui::summary(&report, &cwd);
        }
        Command::Executions { json } => {
            let mut jobs = db.executions()?;
            jobs.sort_by_key(|j| std::cmp::Reverse(j.started_at));
            for j in &mut jobs {
                workflow::refresh(&mut db, j)?;
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&jobs)?);
            } else {
                for j in jobs {
                    println!(
                        "{} · {} · {} · {} · feedback {}",
                        j.id,
                        j.profile_name,
                        j.status,
                        widget::duration(j.wall_ms.unwrap_or(0) as i64),
                        if j.feedback.is_some() {
                            "salvo"
                        } else {
                            "pendente"
                        }
                    );
                }
            }
        }
        Command::Feedback {
            execution,
            delivered,
            speed,
            note,
        } => {
            workflow::save_feedback(
                &mut db,
                &execution,
                workflow::Feedback {
                    delivered,
                    speed,
                    note,
                    recorded_at: Utc::now(),
                },
            )?;
            println!("Feedback salvo com o registro da execução");
        }
        Command::Trend {
            profile,
            benchmark,
            days,
            alpha,
            json,
        } => {
            let mut jobs = db.executions()?;
            for j in &mut jobs {
                workflow::refresh(&mut db, j)?;
            }
            let trends = trend::daily(
                &jobs,
                Utc::now(),
                cli.timezone,
                days,
                alpha,
                profile.as_deref(),
                benchmark.as_deref(),
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&trends)?);
            } else if trends.is_empty() {
                println!("Sem execuções desse perfil. Execute run e registre o feedback.");
            } else {
                for t in trends {
                    println!("{}", trend::render(&t));
                }
                println!(
                    "· = dia sem avaliação. Linhas separadas por perfil, revisão, projeto, benchmark e stack observada."
                );
            }
        }
        Command::Widget(args) => {
            // Resolve a run prefix after first import when starting with an empty database.
            if !args.no_sync && args.filter.run.is_some() {
                collector::sync(&mut db, &sessions)?;
            }
            let scope = args.filter.scope(&db)?;
            widget::run(
                &mut db,
                widget::Options {
                    sessions: &sessions,
                    period: args.period,
                    timezone: cli.timezone,
                    scope,
                    once: args.once,
                    line: args.line,
                    no_sync: args.no_sync,
                    interval: args.interval,
                    quality: args.quality,
                },
            )?;
        }
        Command::Sync => {
            println!(
                "{}",
                serde_json::to_string_pretty(&collector::sync(&mut db, &sessions)?)?
            );
        }
        Command::Report { filter, period } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&analytics::report(
                    &db.snapshot()?,
                    &filter.scope(&db)?,
                    period,
                    Utc::now(),
                    cli.timezone
                )?)?
            );
        }
        Command::Runs { filter, json } => {
            let runs = analytics::runs(&db.snapshot()?, &filter.scope(&db)?);
            if json {
                println!("{}", serde_json::to_string_pretty(&runs)?);
            } else if runs.is_empty() {
                println!("Sem execuções. Rode: stackpulse sync");
            } else {
                for r in runs {
                    println!(
                        "{}  {} tok  {} ativo / {} agentes  {} agentes",
                        widget::clean(&r.id, 36),
                        widget::number(r.metrics.total_tokens),
                        widget::duration(r.metrics.active_ms),
                        widget::duration(r.metrics.agent_ms),
                        r.metrics.agents
                    );
                    println!("  {}", widget::clean(&r.configuration, 240));
                    if let Some(a) = r.annotation {
                        println!(
                            "  {} | {} | qualidade {:?} | {}",
                            widget::clean(&a.label, 80),
                            widget::clean(&a.benchmark, 80),
                            a.quality,
                            if a.baseline { "baseline" } else { "atual" }
                        );
                    }
                    if r.missing_parent {
                        println!("  Parcial: registro do orquestrador ausente");
                    }
                }
            }
        }
        Command::Tag {
            run,
            label,
            benchmark,
            quality,
            baseline,
            current,
        } => {
            let data = db.snapshot()?;
            let id = analytics::resolve_run(&data, &run)?;
            let mut a = data
                .annotations
                .into_iter()
                .find(|a| a.run_id == id)
                .unwrap_or(Annotation {
                    run_id: id.clone(),
                    ..Default::default()
                });
            if let Some(v) = label {
                a.label = v;
            }
            if let Some(v) = benchmark {
                a.benchmark = v;
            }
            if let Some(v) = quality {
                a.quality = Some(v);
            }
            if baseline {
                a.baseline = true;
            } else if current {
                a.baseline = false;
            }
            db.ingest(&Dataset {
                annotations: vec![a],
                ..Default::default()
            })?;
            println!("Anotação salva: {id}");
        }
        Command::BenchmarkCompare { json } => {
            let comparisons =
                analytics::comparisons(&analytics::runs(&db.snapshot()?, &Scope::default()));
            if json {
                println!("{}", serde_json::to_string_pretty(&comparisons)?);
            } else if comparisons.is_empty() {
                println!(
                    "Sem benchmark avaliado. Use tag --benchmark suite@v1 --quality 0.9 --baseline em execuções de referência e --current nas atuais."
                );
            } else {
                for c in comparisons {
                    println!(
                        "{} (baseline {}, atual {})",
                        widget::clean(&c.benchmark, 100),
                        c.baseline_n,
                        c.current_n
                    );
                    println!(
                        "  Qualidade: {:?} pp | IC bootstrap 95%: {:?}",
                        c.quality_change_pp, c.quality_ci95_pp
                    );
                    println!(
                        "  Tokens/qualidade: {:?}% | Tempo mediano: {:?}%",
                        c.tokens_per_quality_change_pct, c.time_median_change_pct
                    );
                    println!("  {}", c.interpretation);
                }
                println!(
                    "Sinais exploratórios não atribuem a causa ao provedor. Mantenha tarefa, contexto, testes, ferramentas e concorrência comparáveis."
                );
            }
        }
        Command::Price {
            provider,
            model,
            service_tier,
            effective_at,
            input,
            cached,
            cache_write,
            output,
            source,
        } => {
            db.ingest(&Dataset {
                prices: vec![Price {
                    provider,
                    model,
                    service_tier,
                    effective_at,
                    input_per_million: input,
                    cached_per_million: cached,
                    cache_write_per_million: cache_write,
                    output_per_million: output,
                    source,
                }],
                ..Default::default()
            })?;
            println!("Tarifa salva. Custos são estimativas de tokens, não valores faturados.");
        }
        Command::Import { file } => {
            let data: Dataset = serde_json::from_reader(std::fs::File::open(file)?)
                .context("JSON normalizado inválido")?;
            let existing = db.snapshot()?;
            for u in &data.usage {
                ensure!(
                    data.sessions
                        .iter()
                        .chain(&existing.sessions)
                        .any(|s| s.id == u.session_id),
                    "Evento sem sessão cadastrada"
                );
            }
            let count = db.ingest(&data)?;
            println!("{count} eventos novos importados.");
        }
    }
    Ok(())
}
