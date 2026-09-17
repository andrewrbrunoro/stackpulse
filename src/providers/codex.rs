//! Native Codex execution and per-run agent roles, without changing saved CLI configuration.
use crate::{profiles::TeamSpec, runner::Request};
use anyhow::{Context, Result, ensure};
use std::{fs::OpenOptions, io::Write, process::Command};
use tempfile::TempDir;

/// The caller must retain the returned directory until Codex and its children finish.
pub fn configure(command: &mut Command, request: &Request<'_>) -> Result<Option<TempDir>> {
    command.args([
        "exec",
        "--json",
        "--color",
        "never",
        "--skip-git-repo-check",
        "--sandbox",
        request.sandbox,
    ]);
    if request.model != "default" {
        command.arg("--model").arg(request.model);
    }
    command.arg("-C").arg(request.cwd);
    let team_files = configure_overrides(command, request)?;
    if let Some(image) = request.image {
        command.arg("--image").arg(image);
    }
    if let Some(schema) = request.output_schema {
        command.arg("--output-schema").arg(schema);
    }
    command.arg("-");
    Ok(team_files)
}

/// A private stdio server retains the CLI's login and the same per-run role layers.
pub(crate) fn configure_app_server(
    command: &mut Command,
    request: &Request<'_>,
) -> Result<Option<TempDir>> {
    command.args(["app-server", "--listen", "stdio://"]);
    configure_overrides(command, request)
}

fn configure_overrides(command: &mut Command, request: &Request<'_>) -> Result<Option<TempDir>> {
    if request.sandbox == "danger-full-access" {
        override_value(
            command,
            "approval_policy",
            toml::Value::String("never".into()),
        );
        override_value(
            command,
            "sandbox_mode",
            toml::Value::String(request.sandbox.into()),
        );
    }
    for (key, value) in [
        ("model_provider", request.provider),
        ("model_reasoning_effort", request.effort),
    ] {
        if value != "default" {
            override_value(command, key, toml::Value::String(value.into()));
        }
    }
    for key in ["agents.enabled", "features.multi_agent"] {
        override_value(command, key, toml::Value::Boolean(request.delegates));
    }
    let team_files = if request.delegates {
        override_value(
            command,
            "agents.max_concurrent_threads_per_session",
            toml::Value::Integer(i64::from(request.settings.max_agents)),
        );
        request
            .team
            .map(|team| configure_team(command, team))
            .transpose()?
            .flatten()
    } else {
        None
    };
    Ok(team_files)
}

fn override_value(command: &mut Command, key: &str, value: toml::Value) {
    command.arg("-c").arg(format!("{key}={value}"));
}

fn configure_team(command: &mut Command, team: &TeamSpec) -> Result<Option<TempDir>> {
    team.validate()?;
    // These are scalar settings under [agents], not legal custom role names.
    const RESERVED: &[&str] = &[
        "enabled",
        "max_threads",
        "max_concurrent_threads_per_session",
        "max_depth",
        "default_subagent_model",
        "default_subagent_reasoning_effort",
        "job_max_runtime_seconds",
        "interrupt_message",
    ];
    for agent in &team.agents {
        ensure!(
            !RESERVED.contains(&agent.role.as_str()),
            "O papel '{}' é reservado pela configuração do Codex; renomeie-o no perfil.",
            agent.role,
        );
    }
    if team.agents.is_empty() {
        return Ok(None);
    }
    let directory = tempfile::Builder::new()
        .prefix("stackpulse-codex-team-")
        .tempdir()
        .context("Não foi possível preparar os papéis do Codex")?;
    for agent in &team.agents {
        let mut layer = toml::Table::new();
        if agent.model != "default" {
            layer.insert("model".into(), toml::Value::String(agent.model.clone()));
        }
        if agent.effort != "default" {
            layer.insert(
                "model_reasoning_effort".into(),
                toml::Value::String(agent.effort.clone()),
            );
        }
        layer.insert(
            "developer_instructions".into(),
            toml::Value::String(crate::team_runtime::child_instructions(team, agent)),
        );
        let path = directory.path().join(format!("{}.toml", agent.role));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&path)
            .with_context(|| format!("Não foi possível preparar o papel {}", agent.role))?;
        file.write_all(toml::to_string(&layer)?.as_bytes())?;
        override_value(
            command,
            &format!("agents.{}.description", agent.role),
            toml::Value::String(format!("{} Quando usar: {}", agent.purpose, agent.when)),
        );
        override_value(
            command,
            &format!("agents.{}.config_file", agent.role),
            toml::Value::String(
                path.to_str()
                    .context("Caminho temporário dos papéis precisa usar UTF-8")?
                    .into(),
            ),
        );
    }
    Ok(Some(directory))
}
