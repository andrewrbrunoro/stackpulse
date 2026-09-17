//! Read-only profile choices for the initial terminal prompt.
use crate::{
    client::Backend,
    profiles::{self, Discovered, Settings, TeamSpec},
    widget,
};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub(crate) struct Choice {
    pub name: String,
    pub scope: String,
    pub path: PathBuf,
    pub subtitle: String,
    pub detail: Vec<String>,
    pub problem: Option<String>,
    pub compiled: bool,
    pub client: String,
    pub model: String,
    pub effort: String,
    pub team: Option<TeamSpec>,
    pub max_agents: u32,
}

impl Choice {
    pub fn team_summary(&self) -> String {
        if let Some(team) = &self.team {
            format!(
                "Equipe configurada: {} {} · {} · /team",
                team.agents.len(),
                if team.agents.len() == 1 {
                    "papel"
                } else {
                    "papéis"
                },
                delegation(&team.delegation)
            )
        } else if self.compiled {
            "Equipe: perfil requer revisão · /team".into()
        } else {
            "Equipe: aguardando extração da imagem · /team".into()
        }
    }

    pub fn team_details(&self) -> Vec<String> {
        let mut details = vec![
            "EQUIPE CONFIGURADA".into(),
            format!("Perfil ativo: {} · escopo {}", self.name, self.scope),
            format!("Arquivo: {}", self.path.display()),
        ];
        if let Some(problem) = &self.problem {
            details.push(format!("Revisão necessária: {problem}"));
        }
        let Some(team) = &self.team else {
            details.push(if self.compiled {
                "A configuração da equipe está indisponível até corrigir o perfil.".into()
            } else {
                "Aguardando extração da imagem no primeiro pedido. Nenhum agente foi iniciado."
                    .into()
            });
            return details;
        };
        details.push(format!(
            "Executor: {} · provider {}",
            self.client,
            team.root_provider()
        ));
        details.push(format!(
            "Delegação: {} · até {} subagentes simultâneos",
            delegation(&team.delegation),
            self.max_agents
        ));
        if team.is_mixed() {
            details.push("Equipe com múltiplos providers: delegação pela ponte StackPulse; cada papel usa seu CLI, modelo e esforço. Autenticação é verificada pelo CLI ao executar.".into());
        }
        if self.client == Backend::Claude.label() && !team.agents.is_empty() && !team.is_mixed() {
            details.push("Claude: limite de concorrência solicitado ao orquestrador, sem imposição nativa pelo adaptador.".into());
            details.push("Equipes Claude exigem workspace-write. read-only com subagentes é recusado, preservando as permissões.".into());
        }
        details.push("Os papéis abaixo pertencem ao perfil; /agents acompanha apenas agentes observados em execução.".into());
        details.push(String::new());
        details.push(format!(
            "Orquestrador {}: {} / {} · provider {}",
            team.orchestrator.role,
            team.orchestrator.model,
            team.orchestrator.effort,
            team.root_provider()
        ));
        details.push(format!("Finalidade: {}", team.orchestrator.purpose));
        details.push(format!("Condição: {}", condition(&team.orchestrator.when)));
        details.push(String::new());
        details.push(format!("Papéis configurados ({})", team.agents.len()));
        for (index, agent) in team.agents.iter().enumerate() {
            details.push(format!(
                "{}. {}: {} / {} · provider {}",
                index + 1,
                agent.role,
                agent.model,
                agent.effort,
                team.provider_for(agent)
            ));
            details.push(format!("Finalidade: {}", agent.purpose));
            details.push(format!("Condição: {}", condition(&agent.when)));
            details.push(String::new());
        }
        details.push(format!(
            "Integração: {}",
            if team.integration.is_empty() {
                "não informada"
            } else {
                &team.integration
            }
        ));
        if !team.notes.is_empty() {
            details.push(format!("Notas: {}", team.notes));
        }
        details.push("PgUp/PgDn para percorrer · Esc para fechar".into());
        details
    }
}

fn delegation(value: &str) -> &str {
    match value {
        "on_demand" => "sob demanda",
        "parallel" => "paralela",
        "sequential" => "sequencial",
        other => other,
    }
}

fn condition(value: &str) -> &str {
    if value.trim().is_empty() {
        "não informada"
    } else {
        value
    }
}

pub(crate) struct Catalog {
    pub choices: Vec<Choice>,
    pub selected: usize,
    pub winner: Option<String>,
    pub notice: String,
}

pub(crate) fn load(config: &Path, cwd: &Path) -> Catalog {
    let settings = match Settings::read(config) {
        Ok(settings) => settings,
        Err(error) => {
            return Catalog {
                choices: Vec::new(),
                selected: 0,
                winner: None,
                notice: format!("Setup necessário. {}", line(&format!("{error:#}"))),
            };
        }
    };
    let discovered = match profiles::discover(&settings, cwd) {
        Ok(discovered) => discovered,
        Err(error) => {
            return Catalog {
                choices: Vec::new(),
                selected: 0,
                winner: None,
                notice: format!(
                    "Não foi possível ler os perfis. {}",
                    line(&format!("{error:#}"))
                ),
            };
        }
    };
    let choices: Vec<_> = discovered
        .into_iter()
        .map(|entry| choice(&settings, entry))
        .collect();
    let selected = settings
        .default_profile
        .as_deref()
        .and_then(|name| choices.iter().position(|choice| choice.name == name))
        .unwrap_or(0);
    let notice = if choices.is_empty() {
        "Nenhum perfil encontrado. Adicione uma imagem de equipe para começar.".into()
    } else if settings
        .default_profile
        .as_ref()
        .is_some_and(|name| !choices.iter().any(|choice| &choice.name == name))
    {
        "O perfil padrão não está disponível neste projeto. Escolha um perfil.".into()
    } else {
        "Escolha o perfil que vai executar seus pedidos neste projeto.".into()
    };
    Catalog {
        choices,
        selected,
        winner: crate::compare::winner(cwd).ok().flatten(),
        notice,
    }
}

fn choice(settings: &Settings, entry: Discovered) -> Choice {
    let mut choice = Choice {
        name: entry.name,
        scope: entry.scope,
        path: entry.path,
        subtitle: "Imagem · perfil será gerado ao enviar".into(),
        detail: Vec::new(),
        problem: None,
        compiled: entry.compiled,
        client: settings.client.label().into(),
        model: "aguardando perfil".into(),
        effort: String::new(),
        team: None,
        max_agents: settings.max_agents,
    };
    choice
        .detail
        .push(format!("Escopo: {}", line(&choice.scope)));
    if !choice.compiled {
        choice.detail.push(choice.subtitle.clone());
        choice.detail.push(format!(
            "Auxiliar de imagem: {} · {} · {}",
            settings.client.label(),
            settings.model,
            settings.effort
        ));
        choice.detail.push(
            "O modelo e os papéis da equipe serão extraídos da imagem no primeiro pedido.".into(),
        );
        return choice;
    }
    choice.subtitle = "Perfil Markdown".into();

    let profile = match profiles::read(&choice.path) {
        Ok((profile, _)) => profile,
        Err(error) => {
            problem(&mut choice, format!("Perfil inválido: {error:#}"));
            return choice;
        }
    };
    let team = &profile.team;
    choice.team = Some(team.clone());
    if team.delegation == "sequential" {
        choice.max_agents = 1;
    }
    // Match workflow::executor_settings without probing or launching a provider.
    let client = if team.is_mixed() {
        Backend::for_provider(team.root_provider())
    } else if team.root_provider() == settings.provider {
        Some(settings.client)
    } else {
        Backend::for_provider(team.root_provider())
            .or((settings.client == Backend::Codex).then_some(Backend::Codex))
    };
    choice.client = client
        .map(|client| client.label().to_string())
        .unwrap_or_else(|| team.root_provider().into());
    choice.model = team.orchestrator.model.clone();
    choice.effort = team.orchestrator.effort.clone();
    choice.subtitle = format!("{} · {} · {}", choice.client, choice.model, choice.effort);
    choice.detail.push(format!(
        "Executor: {} · provider {}",
        choice.client,
        team.root_provider()
    ));
    choice.detail.push(format!(
        "Orquestrador {}: {} · {}",
        team.orchestrator.role, team.orchestrator.model, team.orchestrator.effort
    ));
    choice.detail.push(format!(
        "Delegação: {} · limite de {} agentes",
        delegation(&team.delegation),
        choice.max_agents
    ));
    for agent in team.agents.iter().take(12) {
        choice.detail.push(format!(
            "{}: {} · {} · provider {} — {}",
            agent.role,
            agent.model,
            agent.effort,
            team.provider_for(agent),
            line(&agent.purpose)
        ));
    }
    if team.agents.len() > 12 {
        choice
            .detail
            .push(format!("+ {} papéis no perfil", team.agents.len() - 12));
    }
    if !team.integration.is_empty() {
        choice
            .detail
            .push(format!("Integração: {}", line(&team.integration)));
    }
    if std::iter::once(&team.orchestrator)
        .chain(&team.agents)
        .any(|agent| {
            team.provider_for(agent) == "unknown"
                || agent.model == "unknown"
                || agent.effort == "unknown"
        })
    {
        problem(
            &mut choice,
            "Perfil contém campos unknown; complete o TOML do Markdown antes de executar.".into(),
        );
    } else if let Err(error) = team.validate_execution_providers() {
        problem(&mut choice, error.to_string());
    } else if client.is_none() {
        problem(
            &mut choice,
            format!(
                "Nenhum adaptador para o provider da equipe: {}",
                team.root_provider()
            ),
        );
    } else if matches!(client, Some(Backend::Cursor | Backend::Grok)) && !team.agents.is_empty() {
        let message = if client == Some(Backend::Cursor) {
            "Este adaptador ainda não aplica a configuração dos subagentes do perfil. Use Codex/Claude ou um perfil sem subagentes."
        } else {
            "O Grok pode substituir o modelo/esforço dos subagentes pela configuração local. Este adaptador não garante a equipe do perfil. Use Codex/Claude ou um perfil sem subagentes."
        };
        problem(&mut choice, message.into());
    } else if let Err(error) = profiles::check_image(&profile, &choice.path) {
        problem(&mut choice, format!("{error:#}"));
    }
    choice.detail = choice.detail.iter().map(|text| line(text)).collect();
    choice
}

fn problem(choice: &mut Choice, message: String) {
    let message = line(&message);
    choice.subtitle = format!("Revisão necessária · {}", choice.subtitle);
    choice.detail.push(message.clone());
    choice.problem = Some(message);
}

fn line(text: &str) -> String {
    widget::clean(&text.split_whitespace().collect::<Vec<_>>().join(" "), 260)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::{AgentSpec, Profile, TeamSpec};
    use std::fs;

    fn settings(root: &Path) -> Settings {
        Settings {
            schema_version: 1,
            client: Backend::Claude,
            provider: "anthropic".into(),
            model: "default".into(),
            effort: "default".into(),
            executable: "not-executed-by-profile-catalog".into(),
            providers_root: root.join("providers"),
            default_profile: Some("team".into()),
            max_agents: 4,
            ai_memory: false,
            ai_usagebar: false,
        }
    }

    fn write_profile(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("team.png"), b"image fixture").unwrap();
        let profile = Profile {
            schema_version: 1,
            source_image: "team.png".into(),
            source_sha256: profiles::digest(b"image fixture"),
            generated_by: "fixture".into(),
            generated_at: "2026-09-12T12:00:00Z".parse().unwrap(),
            team: TeamSpec {
                name: "team".into(),
                provider: "openai".into(),
                orchestrator: AgentSpec {
                    provider: None,
                    role: "root".into(),
                    model: "gpt-6-astra".into(),
                    effort: "medium".into(),
                    purpose: "Coordenar".into(),
                    when: "always".into(),
                },
                agents: Vec::new(),
                delegation: "on_demand".into(),
                integration: "Integrar e testar".into(),
                notes: String::new(),
            },
        };
        fs::write(dir.join("team.md"), profiles::markdown(&profile).unwrap()).unwrap();
    }

    #[test]
    fn missing_and_malformed_config_explain_setup_without_creating_files() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.json");
        let missing = load(&config, temp.path());
        assert!(missing.choices.is_empty());
        assert!(missing.notice.starts_with("Setup necessário"));
        assert!(!config.exists());
        fs::write(&config, b"{broken}").unwrap();
        let malformed = load(&config, temp.path());
        assert!(malformed.choices.is_empty());
        assert!(malformed.notice.starts_with("Setup necessário"));
        assert_eq!(fs::read(&config).unwrap(), b"{broken}");
    }

    #[test]
    fn pending_project_image_overrides_global_markdown_and_default_is_highlighted() {
        let temp = tempfile::tempdir().unwrap();
        let settings = settings(temp.path());
        let config = temp.path().join("config.json");
        settings.save(&config).unwrap();
        let global = settings.providers_root.join("project/all");
        write_profile(&global);
        fs::write(global.join("another.png"), b"another image").unwrap();
        let cwd = temp.path().join("my_project");
        fs::create_dir(&cwd).unwrap();
        let specific = settings.providers_root.join("project/my_project");
        fs::create_dir_all(&specific).unwrap();
        fs::write(specific.join("team.png"), b"project image").unwrap();
        let catalog = load(&config, &cwd);
        assert_eq!(catalog.choices.len(), 2);
        assert_eq!(catalog.selected, 1);
        let choice = &catalog.choices[catalog.selected];
        assert_eq!(choice.name, "team");
        assert_eq!(choice.scope, "my_project");
        assert_eq!(choice.path, specific.join("team.png"));
        assert!(!choice.compiled);
        assert!(choice.problem.is_none());
        assert!(choice.subtitle.starts_with("Imagem"));
        assert_eq!(choice.model, "aguardando perfil");
        assert!(choice.team_summary().contains("aguardando extração"));
        assert!(
            choice
                .team_details()
                .iter()
                .any(|line| line.contains("Aguardando extração"))
        );
        assert!(!specific.join("team.md").exists());
    }

    #[test]
    fn executor_details_use_profile_team_instead_of_image_helper() {
        let temp = tempfile::tempdir().unwrap();
        let settings = settings(temp.path());
        let config = temp.path().join("config.json");
        settings.save(&config).unwrap();
        write_profile(&settings.providers_root.join("project/all"));
        let catalog = load(&config, temp.path());
        let choice = &catalog.choices[0];
        assert!(choice.compiled);
        assert!(choice.problem.is_none());
        assert_eq!(choice.client, "Codex CLI");
        assert_eq!(choice.model, "gpt-6-astra");
        assert_eq!(choice.effort, "medium");
        assert!(choice.subtitle.contains("gpt-6-astra"));
        assert!(!choice.subtitle.contains("Claude"));
    }

    #[test]
    fn team_details_include_all_32_roles_full_purposes_conditions_and_integration() {
        let temp = tempfile::tempdir().unwrap();
        let settings = settings(temp.path());
        let config = temp.path().join("config.json");
        settings.save(&config).unwrap();
        let global = settings.providers_root.join("project/all");
        write_profile(&global);
        let path = global.join("team.md");
        let (mut profile, _) = profiles::read(&path).unwrap();
        profile.team.delegation = "parallel".into();
        profile.team.agents = (1..=32)
            .map(|index| AgentSpec {
                provider: None,
                role: format!("role_{index}"),
                model: format!("model-{index}"),
                effort: "high".into(),
                purpose: format!("{} FINALIDADE_{index}", "Descrição completa. ".repeat(20)),
                when: format!("CONDIÇÃO_{index}"),
            })
            .collect();
        fs::write(&path, profiles::markdown(&profile).unwrap()).unwrap();
        let before = fs::read(&path).unwrap();
        let catalog = load(&config, temp.path());
        let choice = &catalog.choices[0];
        assert_eq!(
            choice.team_summary(),
            "Equipe configurada: 32 papéis · paralela · /team"
        );
        let details = choice.team_details().join("\n");
        for agent in &profile.team.agents {
            assert!(details.contains(&format!(
                "{}: {} / {}",
                agent.role, agent.model, agent.effort
            )));
            assert!(details.contains(&agent.purpose));
            assert!(details.contains(&agent.when));
        }
        assert!(details.contains("Orquestrador root: gpt-6-astra / medium"));
        assert!(details.contains("Integração: Integrar e testar"));
        assert_eq!(fs::read(path).unwrap(), before);
    }

    #[test]
    fn mixed_catalog_shows_resolved_providers_and_rejects_unroutable_roles() {
        let temp = tempfile::tempdir().unwrap();
        let settings = settings(temp.path());
        let config = temp.path().join("config.json");
        settings.save(&config).unwrap();
        let global = settings.providers_root.join("project/all");
        write_profile(&global);
        let path = global.join("team.md");
        let (mut profile, _) = profiles::read(&path).unwrap();
        // Root override wins over the team fallback, without probing installed CLIs.
        profile.team.provider = "xai".into();
        profile.team.orchestrator.provider = Some("openai".into());
        let mut child = profile.team.orchestrator.clone();
        child.role = "worker".into();
        child.provider = None;
        profile.team.agents = vec![child];
        fs::write(&path, profiles::markdown(&profile).unwrap()).unwrap();
        let catalog = load(&config, temp.path());
        let choice = &catalog.choices[0];
        assert_eq!(choice.client, "Codex CLI");
        assert!(choice.problem.is_none());
        let details = choice.team_details().join("\n");
        assert!(details.contains("provider openai"));
        assert!(details.contains("provider xai"));
        assert!(details.contains("ponte StackPulse"));
        for provider in ["unknown", "missing_adapter"] {
            profile.team.agents[0].provider = Some(provider.into());
            fs::write(&path, profiles::markdown(&profile).unwrap()).unwrap();
            assert!(load(&config, temp.path()).choices[0].problem.is_some());
        }
    }

    #[test]
    fn cursor_and_grok_teams_explain_adapter_limits_without_claiming_clients_are_missing() {
        let temp = tempfile::tempdir().unwrap();
        let settings = settings(temp.path());
        let config = temp.path().join("config.json");
        settings.save(&config).unwrap();
        let global = settings.providers_root.join("project/all");
        write_profile(&global);
        let path = global.join("team.md");
        let (mut profile, _) = profiles::read(&path).unwrap();
        let mut worker = profile.team.orchestrator.clone();
        worker.role = "worker".into();
        for (provider, client, reason) in [
            ("cursor", "Cursor Agent", "não aplica a configuração"),
            ("xai", "Grok CLI", "pela configuração local"),
        ] {
            profile.team.provider = provider.into();
            profile.team.agents = vec![worker.clone()];
            fs::write(&path, profiles::markdown(&profile).unwrap()).unwrap();
            let catalog = load(&config, temp.path());
            let choice = &catalog.choices[0];
            assert_eq!(choice.client, client);
            assert!(
                choice
                    .problem
                    .as_ref()
                    .unwrap()
                    .contains("Use Codex/Claude ou")
            );
            let details = choice.team_details().join("\n");
            assert!(details.contains("Revisão necessária:"));
            assert!(details.contains(reason));
            profile.team.agents.clear();
            fs::write(&path, profiles::markdown(&profile).unwrap()).unwrap();
            assert!(load(&config, temp.path()).choices[0].problem.is_none());
        }
    }

    #[test]
    fn stale_or_invalid_markdown_stays_visible_with_actionable_problem() {
        let temp = tempfile::tempdir().unwrap();
        let settings = settings(temp.path());
        let config = temp.path().join("config.json");
        settings.save(&config).unwrap();
        let global = settings.providers_root.join("project/all");
        write_profile(&global);
        fs::write(global.join("team.png"), b"changed image").unwrap();
        let catalog = load(&config, temp.path());
        assert_eq!(catalog.choices.len(), 1);
        let problem = catalog.choices[0].problem.as_deref().unwrap();
        assert!(problem.contains("Imagem mudou"));
        assert!(problem.contains("profiles compile"));
        fs::write(global.join("team.md"), b"invalid markdown").unwrap();
        let catalog = load(&config, temp.path());
        assert!(catalog.choices[0].compiled);
        assert!(
            catalog.choices[0]
                .problem
                .as_deref()
                .unwrap()
                .contains("frontmatter")
        );
    }

    #[test]
    fn empty_library_prompts_for_image() {
        let temp = tempfile::tempdir().unwrap();
        let settings = settings(temp.path());
        let config = temp.path().join("config.json");
        settings.save(&config).unwrap();
        let catalog = load(&config, temp.path());
        assert!(catalog.choices.is_empty());
        assert!(catalog.notice.contains("Adicione uma imagem"));
    }
}
