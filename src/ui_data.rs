//! Read-only presentation models for the terminal application.
use crate::{
    analytics::{self, Metrics, Period, Scope},
    db::Db,
    profiles::{self, Settings, TeamSpec},
    trend, widget,
    workflow::Execution,
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use clap::ValueEnum;
use std::{cmp::Reverse, path::Path};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum Page {
    #[default]
    Overview,
    Profiles,
    Run,
    Executions,
    Trend,
    Runs,
    Report,
    Compare,
    Price,
    Import,
    Sync,
    Widget,
    Setup,
}

pub(crate) const PAGES: [Page; 13] = [
    Page::Overview,
    Page::Run,
    Page::Profiles,
    Page::Executions,
    Page::Trend,
    Page::Report,
    Page::Runs,
    Page::Compare,
    Page::Price,
    Page::Import,
    Page::Sync,
    Page::Widget,
    Page::Setup,
];

impl Page {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Overview => "Visão geral",
            Self::Profiles => "Perfis",
            Self::Run => "Executar pedido",
            Self::Executions => "Execuções",
            Self::Trend => "Tendência",
            Self::Runs => "Runs / anotações",
            Self::Report => "Relatório",
            Self::Compare => "Comparar entrega",
            Self::Price => "Tarifas",
            Self::Import => "Importar dados",
            Self::Sync => "Sincronizar",
            Self::Widget => "Widget compacto",
            Self::Setup => "Setup",
        }
    }

    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::Overview => "Consumo, tempo e atividade no seu ambiente",
            Self::Profiles => "Transforme imagens de equipes em perfis executáveis",
            Self::Run => "Envie um pedido usando uma equipe salva",
            Self::Executions => "Stack, duração, tokens e feedback de cada pedido",
            Self::Trend => "Entrega e rapidez ao longo dos últimos 30 dias",
            Self::Runs => "Sessões e agentes agrupados pela execução raiz",
            Self::Report => "Consumo por hora, dia ou mês e por configuração",
            Self::Compare => "Compare amostras equivalentes com uma referência",
            Self::Price => "Preços versionados em USD por milhão de tokens",
            Self::Import => "Adicione um dataset JSON à base local",
            Self::Sync => "Atualize a base com os registros locais do Codex",
            Self::Widget => "Acompanhe consumo ou qualidade em um painel pequeno",
            Self::Setup => "Clientes, modelo auxiliar e limites da equipe",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Item {
    pub id: String,
    pub label: String,
    pub summary: String,
    pub details: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct View {
    pub title: String,
    pub subtitle: String,
    pub lines: Vec<String>,
    pub items: Vec<Item>,
    pub empty: String,
}

impl View {
    fn new(page: Page) -> Self {
        Self {
            title: page.label().into(),
            subtitle: page.description().into(),
            lines: Vec::new(),
            items: Vec::new(),
            empty: "Nenhum registro disponível.".into(),
        }
    }
}

/// Viewing a page never launches a CLI, imports logs, or updates execution records.
pub(crate) fn load(
    page: Page,
    db: &Db,
    config: &Path,
    cwd: &Path,
    timezone: Tz,
    period: Period,
) -> Result<View> {
    let mut view = View::new(page);
    match page {
        Page::Overview | Page::Report => {
            let data = db.snapshot()?;
            let report = analytics::report(&data, &Scope::default(), period, Utc::now(), timezone)?;
            view.subtitle = format!(
                "{} · {} → {} · {}",
                period.label(),
                date(report.from, timezone),
                date(report.to, timezone),
                timezone
            );
            view.lines = metrics_lines(&report.current);
            view.lines.push(format!(
                "Tokens vs. período anterior: {}",
                delta(report.token_change_pct)
            ));
            let peak = report.series.iter().copied().max().unwrap_or(0);
            view.lines.push(format!(
                "Consumo  {}",
                trend::spark(
                    report
                        .series
                        .iter()
                        .map(|v| { (peak > 0).then_some(*v as f64 / peak.max(1) as f64) })
                )
            ));
            view.lines.push("h / d / m muda o período exibido.".into());
            if page == Page::Overview {
                view.lines.push(String::new());
                view.lines.push(format!("Pasta atual: {}", cwd.display()));
                if let Some(settings) = settings(config, &mut view) {
                    view.lines.push(format!(
                        "Auxiliar: {} · {} · {}",
                        settings.client.label(),
                        settings.model,
                        settings.effort
                    ));
                    view.lines.push(format!(
                        "Perfil padrão: {} · limite de {} agentes",
                        settings
                            .default_profile
                            .as_deref()
                            .unwrap_or("seleção automática"),
                        settings.max_agents
                    ));
                }
                let mut jobs = db.executions()?;
                jobs.sort_by_key(|j| Reverse(j.started_at));
                view.lines.push(String::new());
                view.lines.push("PEDIDOS RECENTES".into());
                if jobs.is_empty() {
                    view.lines.push(
                        "Abra Perfis para adicionar sua equipe e Executar pedido para começar."
                            .into(),
                    );
                }
                for job in jobs.iter().take(4) {
                    view.lines.push(format!(
                        "{}  {} · {} · {} tokens",
                        date(job.started_at, timezone),
                        job.profile_name,
                        status(&job.status),
                        execution_tokens(job)
                    ));
                }
            } else {
                view.lines.push(String::new());
                view.lines.push("CONFIGURAÇÕES OBSERVADAS".into());
                if report.configurations.is_empty() {
                    view.lines
                        .push("Nenhum evento no período. Sincronize ou execute um pedido.".into());
                }
                for (config, tokens) in report.configurations {
                    view.lines
                        .push(format!("{} tokens · {config}", widget::number(tokens)));
                }
                view.lines.push(String::new());
                view.lines.push(
                    "Tempo ativo une intervalos simultâneos; tempo dos agentes soma cada agente."
                        .into(),
                );
            }
            view.empty.clear();
        }
        Page::Profiles => {
            view.empty =
                "Nenhum perfil nesta pasta. Adicione uma imagem para criar sua equipe.".into();
            if let Some(settings) = settings(config, &mut view) {
                view.lines
                    .push(format!("Biblioteca: {}", settings.providers_root.display()));
                view.lines
                    .push("Perfis do projeto têm prioridade sobre os perfis de all.".into());
                for found in profiles::discover(&settings, cwd)? {
                    let mut details = vec![
                        format!("Nome: {}", found.name),
                        format!("Escopo: {}", found.scope),
                        format!("Arquivo: {}", found.path.display()),
                    ];
                    let state = if found.compiled {
                        match profiles::read(&found.path) {
                            Ok((profile, _)) => {
                                details.push(format!("Imagem: {}", profile.source_image));
                                details.push(format!(
                                    "Gerado: {} · {}",
                                    date(profile.generated_at, timezone),
                                    profile.generated_by
                                ));
                                details.extend(team_lines(&profile.team));
                                match profiles::check_image(&profile, &found.path) {
                                    Ok(()) => {
                                        let incomplete = profile.team.provider == "unknown"
                                            || std::iter::once(&profile.team.orchestrator)
                                                .chain(&profile.team.agents)
                                                .any(|a| {
                                                    a.model == "unknown" || a.effort == "unknown"
                                                });
                                        if incomplete {
                                            details.push("Preencha os campos unknown no Markdown antes de executar.".into());
                                            "campos a completar"
                                        } else {
                                            "pronto"
                                        }
                                    }
                                    Err(error) => {
                                        details.push(format!("Origem: {error:#}"));
                                        "recompilar imagem"
                                    }
                                }
                            }
                            Err(error) => {
                                details.push(format!("Perfil inválido: {error:#}"));
                                "Markdown inválido"
                            }
                        }
                    } else {
                        details.push(
                            "Compile a imagem com o auxiliar configurado para gerar o Markdown."
                                .into(),
                        );
                        "imagem a compilar"
                    };
                    details.insert(3, format!("Estado: {state}"));
                    view.items.push(Item {
                        id: found.path.to_string_lossy().into_owned(),
                        label: found.name,
                        summary: format!("{} · {state}", found.scope),
                        details,
                    });
                }
            }
        }
        Page::Executions => {
            view.empty = "Nenhum pedido registrado. Abra Executar pedido para começar.".into();
            view.lines
                .push("Selecione uma execução para ver detalhes ou registrar feedback.".into());
            let mut jobs = db.executions()?;
            jobs.sort_by_key(|j| Reverse(j.started_at));
            view.items = jobs
                .iter()
                .map(|job| execution_item(job, timezone))
                .collect();
        }
        Page::Runs => {
            view.empty = "Nenhuma sessão registrada. Sincronize logs ou execute um pedido.".into();
            view.lines.push(
                "Selecione uma run para anotar qualidade, benchmark e grupo de comparação.".into(),
            );
            view.items = analytics::runs(&db.snapshot()?, &Scope::default())
                .into_iter()
                .map(|run| {
                    let mut details = vec![
                        format!("ID: {}", run.id),
                        format!("Projeto: {}", run.project),
                        format!("Início: {}", date(run.first_seen, timezone)),
                        format!("Último evento: {}", date(run.last_seen, timezone)),
                        format!("Stack observada: {}", run.configuration),
                    ];
                    details.extend(metrics_lines(&run.metrics));
                    if run.missing_parent {
                        details
                            .push("Cobertura parcial: sessão raiz ausente nos registros.".into());
                    }
                    if let Some(annotation) = run.annotation {
                        details.extend([
                            format!("Rótulo: {}", or_dash(&annotation.label)),
                            format!("Benchmark: {}", or_dash(&annotation.benchmark)),
                            format!("Qualidade: {}", quality(annotation.quality)),
                            format!(
                                "Grupo: {}",
                                if annotation.baseline {
                                    "referência"
                                } else {
                                    "atual"
                                }
                            ),
                        ]);
                    } else {
                        details.push(
                            "Sem anotação. Registre qualidade e benchmark para comparar.".into(),
                        );
                    }
                    Item {
                        id: run.id,
                        label: run.label,
                        summary: format!(
                            "{} · {} tokens · {} agentes",
                            date(run.last_seen, timezone),
                            metric_tokens(&run.metrics),
                            run.metrics.agents
                        ),
                        details,
                    }
                })
                .collect();
        }
        Page::Trend => {
            view.empty =
                "Nenhum pedido concluído nos últimos 30 dias. Execute e avalie uma entrega.".into();
            view.lines = vec![
                "Softline: média móvel exponencial (α = 0,3). · indica ausência de feedback.".into(),
                "Grupos preservam perfil, projeto, stack e cobertura; a curva não prova alteração do provedor.".into(),
            ];
            let mut trends =
                trend::daily(&db.executions()?, Utc::now(), timezone, 30, 0.3, None, None)?;
            trends.sort_by_key(|t| Reverse(t.last_execution));
            view.items = trends
                .into_iter()
                .map(|t| {
                    let mut details: Vec<String> =
                        trend::render(&t).lines().map(str::to_owned).collect();
                    details.extend([
                        format!("Projeto: {}", t.project),
                        format!("Benchmark: {}", or_dash(&t.benchmark)),
                        format!(
                            "Stack observada: {}",
                            t.observed_stack.as_deref().unwrap_or("não informada")
                        ),
                        format!(
                            "Cobertura: {} · limite de {} agentes",
                            coverage(&t.coverage),
                            t.max_agents
                        ),
                        String::new(),
                        "DIAS COM EXECUÇÕES".into(),
                    ]);
                    details.extend(t.points.iter().filter(|p| p.executions > 0).map(|p| {
                        format!(
                            "{} · {} pedidos · {} avaliações · entrega {} · {} tokens · {}",
                            p.day,
                            p.executions,
                            p.rated,
                            quality(p.delivery),
                            p.tokens
                                .map(widget::number)
                                .unwrap_or_else(|| "não informados".into()),
                            usd(p.estimated_usd)
                        )
                    }));
                    Item {
                        id: t.cohort_id.clone(),
                        label: format!("{} · grupo {}", t.profile, t.cohort_id),
                        summary: format!("{} feedbacks · {}", t.feedback_count, t.signal),
                        details,
                    }
                })
                .collect();
        }
        Page::Compare => {
            view.empty =
                "Anote runs com o mesmo benchmark e stack, qualidade e grupos referência / atual."
                    .into();
            view.lines = vec![
                "Qualidade e tempo são comparados dentro do mesmo benchmark e stack.".into(),
                "São necessárias ao menos 5 amostras por grupo para o sinal exploratório.".into(),
                "Mudança de consumo ou latência, isoladamente, não mede capacidade.".into(),
            ];
            view.items =
                analytics::comparisons(&analytics::runs(&db.snapshot()?, &Scope::default()))
                    .into_iter()
                    .map(|c| {
                        let mut details = vec![
                            format!("Benchmark: {}", c.benchmark),
                            format!("Stack: {}", c.configuration),
                            format!(
                                "Amostras: {} referência · {} atuais",
                                c.baseline_n, c.current_n
                            ),
                            format!(
                                "Entrega: {}",
                                c.quality_change_pp
                                    .map(|v| format!("{v:+.1} pp"))
                                    .unwrap_or_else(|| "sem amostras nos dois grupos".into())
                            ),
                            format!(
                                "Tokens por entrega: {}",
                                delta(c.tokens_per_quality_change_pct)
                            ),
                            format!(
                                "Mediana do tempo ativo: {}",
                                delta(c.time_median_change_pct)
                            ),
                        ];
                        if let Some([low, high]) = c.quality_ci95_pp {
                            details.push(format!(
                                "IC 95% da diferença de entrega: {low:+.1} a {high:+.1} pp"
                            ));
                        }
                        details.push(c.interpretation.clone());
                        details.push(
                            "Um sinal pede investigação e não atribui a causa ao provedor.".into(),
                        );
                        Item {
                            id: profiles::digest(
                                format!("{}\0{}", c.benchmark, c.configuration).as_bytes(),
                            ),
                            label: c.benchmark,
                            summary: format!(
                                "{} / {} amostras · {}",
                                c.baseline_n, c.current_n, c.interpretation
                            ),
                            details,
                        }
                    })
                    .collect();
        }
        Page::Price => {
            view.empty =
                "Nenhuma tarifa cadastrada. Adicione preços e a fonte para estimar custo.".into();
            view.lines = vec![
                "Valores em USD por 1 milhão de tokens, com data de vigência e fonte.".into(),
                "Estimativas por token não representam a cobrança do plano assinado.".into(),
                "Cada evento usa a tarifa vigente para provider, modelo e service tier.".into(),
            ];
            let mut prices = db.snapshot()?.prices;
            prices.sort_by_key(|p| (p.provider.clone(), p.model.clone(), Reverse(p.effective_at)));
            view.items = prices
                .into_iter()
                .map(|p| Item {
                    id: format!(
                        "{}\0{}\0{}\0{}",
                        p.provider, p.model, p.service_tier, p.effective_at
                    ),
                    label: format!("{} · {}", p.provider, p.model),
                    summary: format!(
                        "{} · desde {}",
                        p.service_tier,
                        date(p.effective_at, timezone)
                    ),
                    details: vec![
                        format!("Provider: {}", p.provider),
                        format!("Modelo: {}", p.model),
                        format!("Service tier: {}", p.service_tier),
                        format!("Vigente desde: {}", p.effective_at.to_rfc3339()),
                        format!("Entrada: USD {:.6} / milhão", p.input_per_million),
                        format!("Leitura de cache: USD {:.6} / milhão", p.cached_per_million),
                        format!(
                            "Escrita de cache: USD {:.6} / milhão",
                            p.cache_write_per_million
                        ),
                        format!("Saída: USD {:.6} / milhão", p.output_per_million),
                        format!("Fonte: {}", p.source),
                    ],
                })
                .collect();
        }
        Page::Run => {
            view.lines = vec![
                "1. Escolha um perfil disponível na pasta atual.".into(),
                "2. Escreva o pedido e ajuste benchmark, permissões e tempo limite.".into(),
                "3. Execute; acompanhe a saída e avalie a entrega em Execuções.".into(),
                String::new(),
            ];
            if let Some(settings) = settings(config, &mut view) {
                view.lines.push(format!(
                    "Auxiliar: {} · {} · {}",
                    settings.client.label(),
                    settings.model,
                    settings.effort
                ));
                view.lines
                    .push(format!("Pasta de trabalho: {}", cwd.display()));
                view.lines
                    .push(format!("Limite de agentes: {}", settings.max_agents));
                match profiles::discover(&settings, cwd) {
                    Ok(profiles) if profiles.is_empty() => view
                        .lines
                        .push("Adicione uma imagem em Perfis antes do primeiro pedido.".into()),
                    Ok(profiles) => {
                        view.lines.push(format!(
                            "Perfis: {}",
                            profiles
                                .iter()
                                .map(|p| p.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                        view.lines.push(
                            "O cliente de execução é escolhido pelo provider da equipe.".into(),
                        );
                        view.lines.push(
                            "Uma imagem ainda sem Markdown será compilada ao executar.".into(),
                        );
                    }
                    Err(error) => view
                        .lines
                        .push(format!("Não foi possível listar perfis: {error:#}")),
                }
            }
            view.empty.clear();
        }
        Page::Import => {
            view.lines = vec![
                "Escolha um arquivo JSON compatível com o formato Dataset.".into(),
                "Ele pode conter sessions, usage, turns, annotations e prices.".into(),
                "A importação valida referências, tokens, notas e preços antes de gravar.".into(),
                "Registros existentes são conciliados por seus identificadores.".into(),
                "Os dados ficam no SQLite local selecionado para esta interface.".into(),
            ];
            view.empty.clear();
        }
        Page::Sync => {
            let data = db.snapshot()?;
            view.lines = vec![
                "Importa a pasta de sessões local do Codex configurada em --sessions.".into(),
                "Usa os eventos existentes para registrar tokens, tempo e árvore de agentes."
                    .into(),
                "Claude, Cursor e Grok são rastreados nos pedidos executados pelo timeline.".into(),
                "Abrir esta página não inicia sincronização nem chama uma LLM.".into(),
                String::new(),
                format!(
                    "Na base: {} sessões · {} eventos · {} turnos",
                    data.sessions.len(),
                    data.usage.len(),
                    data.turns.len()
                ),
            ];
            view.empty.clear();
        }
        Page::Widget => {
            view.lines = vec![
                "Abra o painel compacto de consumo ou de qualidade no terminal.".into(),
                "Consumo: tokens, tempo, custo estimado e histórico por hora, dia ou mês.".into(),
                "Qualidade: entrega, rapidez e softline do grupo mais recente.".into(),
                "Teclas do widget: h / d / m período · g qualidade · q voltar.".into(),
                "A opção de sincronização importa logs locais do Codex durante a atualização."
                    .into(),
            ];
            view.empty.clear();
        }
        Page::Setup => {
            view.lines
                .push(format!("Configuração: {}", config.display()));
            if let Some(settings) = settings(config, &mut view) {
                view.lines.extend([
                    format!("Cliente auxiliar: {}", settings.client.label()),
                    format!("Provider: {}", settings.provider),
                    format!("Modelo: {} · esforço {}", settings.model, settings.effort),
                    format!("Executável: {}", settings.executable.display()),
                    format!("Biblioteca: {}", settings.providers_root.display()),
                    format!(
                        "Perfil padrão: {}",
                        settings.default_profile.as_deref().unwrap_or("automático")
                    ),
                    format!("Limite de agentes: {}", settings.max_agents),
                ]);
            }
            view.lines.push(String::new());
            view.lines.push(
                "Abra o assistente para detectar CLIs instalados e revisar a configuração.".into(),
            );
            view.lines
                .push("As execuções reutilizam o login do cliente instalado.".into());
            view.empty.clear();
        }
    }
    Ok(view)
}

fn settings(path: &Path, view: &mut View) -> Option<Settings> {
    if !path.exists() {
        view.lines.push(
            "Configuração inicial ausente. Abra Setup para escolher seu cliente auxiliar.".into(),
        );
        return None;
    }
    match Settings::read(path) {
        Ok(settings) => Some(settings),
        Err(error) => {
            view.lines.push(format!("Configuração inválida: {error:#}"));
            view.lines.push(format!(
                "Revise o arquivo {} ou abra Setup.",
                path.display()
            ));
            None
        }
    }
}

fn date(at: DateTime<Utc>, timezone: Tz) -> String {
    at.with_timezone(&timezone)
        .format("%d/%m %H:%M")
        .to_string()
}

fn usd(value: Option<f64>) -> String {
    value
        .map(|v| format!("USD {v:.4} estimado"))
        .unwrap_or_else(|| "custo indisponível".into())
}

fn delta(value: Option<f64>) -> String {
    value
        .map(|v| format!("{v:+.1}%"))
        .unwrap_or_else(|| "sem base de comparação".into())
}

fn quality(value: Option<f64>) -> String {
    value
        .map(|v| format!("{:.0}%", v * 100.0))
        .unwrap_or_else(|| "não avaliada".into())
}

fn or_dash(value: &str) -> &str {
    if value.is_empty() { "—" } else { value }
}

fn metric_tokens(metrics: &Metrics) -> String {
    if metrics.events == 0 {
        "não informados".into()
    } else {
        widget::number(metrics.total_tokens)
    }
}

fn metrics_lines(metrics: &Metrics) -> Vec<String> {
    let measured_time =
        metrics.agent_ms > 0 || (metrics.events > 0 && metrics.untimed_events < metrics.events);
    let duration = |value| {
        if measured_time {
            widget::duration(value)
        } else {
            "indisponível".into()
        }
    };
    let mut lines = vec![
        format!(
            "TOKENS  {}     CUSTO  {}",
            metric_tokens(metrics),
            usd(metrics.estimated_usd)
        ),
        format!(
            "TEMPO ATIVO  {}     TEMPO DOS AGENTES  {}",
            duration(metrics.active_ms),
            duration(metrics.agent_ms)
        ),
        format!(
            "{} agentes · {} runs · {} eventos",
            metrics.agents, metrics.runs, metrics.events
        ),
        format!(
            "Cobertura de preço: {}/{} eventos",
            metrics.priced_events, metrics.events
        ),
    ];
    if metrics.events > 0 {
        lines.push(format!(
            "Entrada {} · saída {} · cache lido {} · cache gravado {} · raciocínio {}",
            widget::number(metrics.tokens.input_tokens),
            widget::number(metrics.tokens.output_tokens),
            widget::number(metrics.tokens.cached_input_tokens),
            widget::number(metrics.tokens.cache_write_input_tokens),
            widget::number(metrics.tokens.reasoning_output_tokens)
        ));
    }
    if metrics.open_turns > 0 || metrics.untimed_events > 0 {
        lines.push(format!(
            "Tempo parcial: {} turnos abertos · {} eventos sem turno",
            metrics.open_turns, metrics.untimed_events
        ));
    }
    lines
}

fn execution_tokens(job: &Execution) -> String {
    job.metrics
        .as_ref()
        .filter(|m| m.events > 0)
        .map(|m| m.total_tokens)
        .or(job.reported_tokens.map(|t| t.total()))
        .map(widget::number)
        .unwrap_or_else(|| "não informados".into())
}

fn status(value: &str) -> &str {
    match value {
        "completed" => "concluída",
        "running" => "em execução",
        "failed" => "falhou",
        "interrupted" => "interrompida",
        "timeout" | "timed_out" => "tempo esgotado",
        other => other,
    }
}

fn coverage(value: &str) -> &str {
    match value {
        "local_observed" => "árvore observada nos logs locais",
        "local_partial" => "logs locais parciais",
        "root_only" => "somente agente principal",
        "cli_tree" => "total da árvore informado pelo CLI",
        "cli_partial" => "parcial informado pelo CLI",
        "unavailable" => "não informada",
        other => other,
    }
}

fn team_lines(team: &TeamSpec) -> Vec<String> {
    let mut lines = vec![
        format!("Equipe: {} · provider {}", team.name, team.provider),
        format!("Delegação: {}", team.delegation),
        "STACK PLANEJADA".into(),
    ];
    for agent in std::iter::once(&team.orchestrator).chain(&team.agents) {
        lines.push(format!(
            "{} → {} · {}",
            agent.role, agent.model, agent.effort
        ));
        lines.push(format!("  {}", agent.purpose));
        if !agent.when.is_empty() {
            lines.push(format!("  Quando: {}", agent.when));
        }
    }
    if !team.integration.is_empty() {
        lines.push(format!("Integração: {}", team.integration));
    }
    if !team.notes.is_empty() {
        lines.push(format!("Notas: {}", team.notes));
    }
    lines
}

fn execution_item(job: &Execution, timezone: Tz) -> Item {
    let elapsed = job
        .wall_ms
        .map(|ms| widget::duration(ms.min(i64::MAX as u64) as i64))
        .unwrap_or_else(|| "ainda não registrado".into());
    let mut details = vec![
        format!("ID: {}", job.id),
        format!("Estado: {}", status(&job.status)),
        format!(
            "Perfil: {} · revisão {}",
            job.profile_name, job.profile_sha256
        ),
        format!("Projeto: {}", job.project),
        format!("Início: {}", date(job.started_at, timezone)),
        format!(
            "Fim: {}",
            job.ended_at
                .map(|at| date(at, timezone))
                .unwrap_or_else(|| "ainda não registrado".into())
        ),
        format!(
            "Duração total: {elapsed} · tokens: {}",
            execution_tokens(job)
        ),
        format!(
            "Custo: {}",
            usd(job
                .metrics
                .as_ref()
                .and_then(|m| m.estimated_usd)
                .or(job.reported_cost_usd))
        ),
        format!("Cliente executor: {}", job.execution_client.label()),
        format!(
            "Modelo observado: {}",
            job.observed_model.as_deref().unwrap_or("não informado")
        ),
        format!(
            "Stack observada: {}",
            job.observed_stack.as_deref().unwrap_or("não informada")
        ),
        format!("Cobertura: {}", coverage(&job.coverage)),
        format!("Benchmark: {}", or_dash(&job.benchmark)),
        format!(
            "Permissões: {} · limite de {} agentes",
            job.sandbox, job.max_agents
        ),
        format!(
            "Pedido: {} caracteres · hash {}",
            job.prompt_chars, job.prompt_sha256
        ),
    ];
    if let Some(feedback) = &job.feedback {
        details.push(format!(
            "Feedback: entrega {} · rapidez {} · {}",
            quality(Some(feedback.delivered)),
            if (1..=5).contains(&feedback.speed) {
                format!("{}/5", feedback.speed)
            } else {
                "sem avaliação".into()
            },
            date(feedback.recorded_at, timezone)
        ));
        if !feedback.note.is_empty() {
            details.push(format!("Nota: {}", feedback.note));
        }
    } else {
        details.push("Feedback: ainda não registrado".into());
    }
    if let Some(error) = &job.error {
        details.push(format!("Erro: {error}"));
    }
    if let Some(root) = &job.root_id {
        details.push(format!("Run raiz: {root}"));
    }
    details.push(String::new());
    details.extend(team_lines(&job.planned_stack));
    Item {
        id: job.id.clone(),
        label: format!("{} · {}", job.profile_name, date(job.started_at, timezone)),
        summary: format!(
            "{} · {elapsed} · {} tokens · {}",
            status(&job.status),
            execution_tokens(job),
            if job.feedback.is_some() {
                "avaliada"
            } else {
                "sem feedback"
            }
        ),
        details,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Dataset, Session, Tokens, Usage};

    #[test]
    fn empty_views_do_not_require_configuration_or_invent_metrics() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("usage.sqlite")).unwrap();
        let config = dir.path().join("missing.json");
        for page in PAGES {
            let view = load(page, &db, &config, dir.path(), chrono_tz::UTC, Period::Day).unwrap();
            assert!(!view.title.is_empty());
            if matches!(page, Page::Overview | Page::Report) {
                let text = view.lines.join("\n");
                assert!(text.contains("não informados"));
                assert!(text.contains("custo indisponível"));
                assert!(!text.contains("USD 0.0000"));
            }
        }
        assert!(!config.exists());
        assert!(db.snapshot().unwrap().sessions.is_empty());
        assert!(db.executions().unwrap().is_empty());
    }

    #[test]
    fn malformed_configuration_is_a_visible_error_and_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("usage.sqlite")).unwrap();
        let config = dir.path().join("config.json");
        std::fs::write(&config, b"invalid-json").unwrap();
        let view = load(
            Page::Profiles,
            &db,
            &config,
            dir.path(),
            chrono_tz::UTC,
            Period::Day,
        )
        .unwrap();
        assert!(view.lines.join("\n").contains("Configuração inválida"));
        assert_eq!(std::fs::read(&config).unwrap(), b"invalid-json");
    }

    #[test]
    fn run_selection_uses_full_id_and_unmeasured_cost_and_time_stay_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&dir.path().join("usage.sqlite")).unwrap();
        let now = Utc::now();
        db.ingest(&Dataset {
            sessions: vec![Session {
                id: "same-prefix-full-run-id".into(),
                parent_id: None,
                name: "test".into(),
                project: "/project".into(),
                provider: "test".into(),
                created_at: now,
                source: "fixture".into(),
            }],
            usage: vec![Usage {
                id: "usage-1".into(),
                session_id: "same-prefix-full-run-id".into(),
                turn_id: None,
                at: now,
                model: "fixture".into(),
                effort: "default".into(),
                service_tier: "unknown".into(),
                tokens: Tokens {
                    input_tokens: 10,
                    ..Default::default()
                },
            }],
            ..Default::default()
        })
        .unwrap();
        let view = load(
            Page::Runs,
            &db,
            &dir.path().join("config.json"),
            dir.path(),
            chrono_tz::UTC,
            Period::Day,
        )
        .unwrap();
        assert_eq!(view.items.len(), 1);
        assert_eq!(view.items[0].id, "same-prefix-full-run-id");
        let details = view.items[0].details.join("\n");
        assert!(details.contains("custo indisponível"));
        assert!(details.contains("TEMPO ATIVO  indisponível"));
    }
}
