//! Concurrent, isolated profile comparisons and their persisted verdicts.

use crate::{
    db::Db,
    profiles::{self, Settings},
    workflow::{self, Execution},
};
use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const REPORT_VERSION: u32 = 1;
const OUTPUT_LIMIT: usize = 16 * 1024;

pub struct CompareOptions<'a> {
    pub cwd: &'a Path,
    pub sessions: &'a Path,
    pub profiles: &'a [String],
    pub prompt: &'a str,
    pub timeout_secs: u64,
    /// Run after each profile in its private workspace. Without it delivery is inconclusive.
    pub validation_command: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Validated,
    Rejected,
    Inconclusive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub command: String,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub wall_ms: u64,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileResult {
    pub profile_name: String,
    pub workspace: PathBuf,
    pub execution_id: Option<String>,
    pub cli_session_id: Option<String>,
    pub execution_status: String,
    pub final_message: String,
    pub delivery: Delivery,
    /// Complete profile run, including its validation command when present.
    pub wall_ms: u64,
    pub execution_wall_ms: Option<u64>,
    pub tokens: Option<u64>,
    pub token_coverage: String,
    pub tokens_complete: bool,
    pub validation: Option<ValidationResult>,
    pub error: Option<String>,
    /// Equal-weight normalized time/token efficiency among validated deliveries.
    pub ranking_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonReport {
    pub schema_version: u32,
    pub id: String,
    /// Absolute delivery directory; empty only for reports written before this field existed.
    #[serde(default)]
    pub output_dir: PathBuf,
    pub prompt_sha256: String,
    pub prompt_chars: usize,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub validation_command: Option<String>,
    pub results: Vec<ProfileResult>,
    pub winner: Option<String>,
    pub ranking: String,
}

/// Execute one fresh session per profile. Every process receives its own workspace copy.
pub fn run(
    db: &mut Db,
    settings: &Settings,
    options: CompareOptions<'_>,
) -> Result<ComparisonReport> {
    ensure!(options.profiles.len() >= 2, "Selecione ao menos 2 perfis");
    ensure!(!options.prompt.trim().is_empty(), "Prompt vazio");
    ensure!(options.timeout_secs > 0, "Timeout deve ser maior que zero");
    let mut unique = HashSet::new();
    for name in options.profiles {
        profiles::identifier(name)?;
        ensure!(unique.insert(name), "Perfil repetido: {name}");
        let discovered = profiles::resolve(settings, options.cwd, Some(name))?;
        if !discovered.compiled {
            workflow::compile(settings, &discovered.path, options.timeout_secs, false)?;
        }
        // Fail before starting any paid work when a selected profile is unavailable.
        profiles::load_for_run(settings, options.cwd, Some(name))?;
    }
    if let Some(command) = options.validation_command {
        ensure!(!command.trim().is_empty(), "Comando de validação vazio");
    }

    let source = options
        .cwd
        .canonicalize()
        .with_context(|| format!("Workspace inválido: {}", options.cwd.display()))?;
    ensure!(source.is_dir(), "Workspace precisa ser uma pasta");
    let started_at = Utc::now();
    let id = uuid::Uuid::new_v4().to_string();
    let comparison_dir = create_output_dir(&source)?;
    eprintln!("Arquivos do compare: {}", comparison_dir.display());
    let outcome = (|| -> Result<ComparisonReport> {
        let project_key = profiles::project_key(&source)?;
        let mut prepared = Vec::with_capacity(options.profiles.len());
        for profile_name in options.profiles.iter().cloned() {
            let profile_dir = comparison_dir.join(&profile_name);
            // Keep the original project basename so project-scoped profiles still resolve.
            let workspace = profile_dir.join("workspace").join(&project_key);
            let local_state = profile_dir.join("state");
            fs::create_dir_all(&local_state)?;
            copy_workspace(&source, &workspace)?;
            prepared.push((profile_name, workspace, local_state));
        }
        // All snapshots exist before any worker starts, so each profile sees identical input.
        let mut handles = Vec::with_capacity(prepared.len());
        for (profile_name, workspace, local_state) in prepared {
            let settings = settings.clone();
            let sessions = options.sessions.to_path_buf();
            let prompt = options.prompt.to_owned();
            let benchmark = format!("compare:{id}");
            let validation_command = options.validation_command.map(str::to_owned);
            let timeout_secs = options.timeout_secs;
            handles.push((
                profile_name.clone(),
                workspace.clone(),
                thread::spawn(move || {
                    run_profile(
                        settings,
                        sessions,
                        profile_name,
                        prompt,
                        benchmark,
                        timeout_secs,
                        validation_command,
                        workspace,
                        local_state,
                    )
                }),
            ));
        }

        let mut results = Vec::with_capacity(handles.len());
        let mut executions = Vec::new();
        for (profile_name, workspace, handle) in handles {
            match handle.join() {
                Ok(Ok((result, execution))) => {
                    if let Some(execution) = execution {
                        executions.push(execution);
                    }
                    results.push(result);
                }
                Ok(Err(error)) => {
                    results.push(failed_result(profile_name, workspace, error.to_string()))
                }
                Err(_) => results.push(failed_result(
                    profile_name,
                    workspace,
                    "Thread de comparação terminou inesperadamente".into(),
                )),
            }
        }
        // Stable output makes JSON reports and UI rows reproducible despite completion order.
        results.sort_by(|a, b| a.profile_name.cmp(&b.profile_name));
        for execution in &executions {
            db.save_execution(execution)?;
        }
        let selected_winner = rank(&mut results);
        let report = ComparisonReport {
            schema_version: REPORT_VERSION,
            id,
            output_dir: comparison_dir.clone(),
            prompt_sha256: profiles::digest(options.prompt.as_bytes()),
            prompt_chars: options.prompt.chars().count(),
            started_at,
            ended_at: Utc::now(),
            validation_command: options.validation_command.map(str::to_owned),
            results,
            winner: selected_winner,
            ranking: "entrega validada; depois média normalizada de tempo e tokens".into(),
        };
        persist(options.cwd, &comparison_dir, &report)?;
        Ok(report)
    })();
    outcome.with_context(|| {
        format!(
            "Arquivos do compare preservados em {}",
            comparison_dir.display()
        )
    })
}

/// Create a durable directory only after the caller has obtained write consent.
fn create_output_dir(source: &Path) -> Result<PathBuf> {
    let temporary_root = std::env::temp_dir()
        .canonicalize()
        .context("Não foi possível localizar a pasta temporária")?;
    ensure!(
        !temporary_root.starts_with(source),
        "O workspace não pode conter a pasta temporária do sistema"
    );
    for _ in 0..10 {
        let name = format!(
            "stackpulse-{}-compare",
            Utc::now().format("%Y%m%d-%H%M%S-%9f")
        );
        let directory = temporary_root.join(name);
        match fs::create_dir(&directory) {
            Ok(()) => return Ok(directory),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Não foi possível criar {}", directory.display()));
            }
        }
    }
    bail!("Não foi possível criar uma pasta exclusiva para o compare")
}

/// Winner of the most recently completed comparison in this workspace.
pub fn winner(cwd: &Path) -> Result<Option<String>> {
    let path = reports_dir(cwd).join("latest.json");
    if !path.exists() {
        return Ok(None);
    }
    let report: ComparisonReport = serde_json::from_slice(
        &fs::read(&path).with_context(|| format!("Não foi possível ler {}", path.display()))?,
    )?;
    ensure!(
        report.schema_version == REPORT_VERSION,
        "Versão de comparação não suportada"
    );
    Ok(report.winner)
}

fn run_profile(
    settings: Settings,
    sessions: PathBuf,
    profile_name: String,
    prompt: String,
    benchmark: String,
    timeout_secs: u64,
    validation_command: Option<String>,
    workspace: PathBuf,
    local_state: PathBuf,
) -> Result<(ProfileResult, Option<Execution>)> {
    let total = Instant::now();
    let mut local_db = Db::open(&local_state.join("comparison.sqlite"))?;
    let (execution, final_message) = workflow::run_captured(
        &mut local_db,
        &settings,
        workflow::RunOptions {
            cwd: &workspace,
            sessions: &sessions,
            profile: Some(&profile_name),
            prompt: &prompt,
            benchmark: &benchmark,
            sandbox: "workspace-write",
            timeout_secs,
            dry_run: false,
            no_feedback: true,
        },
    )?;
    let execution = execution.context("A execução do perfil não produziu registro")?;

    let validation = validation_command
        .as_deref()
        .map(|command| validate(&workspace, command, timeout_secs))
        .transpose()?;
    let delivery = match &validation {
        Some(result) if result.success && execution.status == "completed" => Delivery::Validated,
        Some(_) => Delivery::Rejected,
        None => Delivery::Inconclusive,
    };
    let error = execution.error.clone().or_else(|| {
        validation
            .as_ref()
            .filter(|result| !result.success)
            .map(|result| {
                if result.timed_out {
                    "Validação excedeu o tempo limite".into()
                } else {
                    format!("Validação terminou com status {:?}", result.exit_code)
                }
            })
    });
    let tokens = execution_tokens(&execution);
    let tokens_complete = tokens.is_some()
        && (execution.planned_stack.agents.is_empty()
            || matches!(execution.coverage.as_str(), "local_observed" | "cli_tree"));
    let measured_wall_ms = total.elapsed().as_millis() as u64;
    let result = ProfileResult {
        profile_name,
        workspace,
        execution_id: Some(execution.id.clone()),
        cli_session_id: execution.cli_session_id.clone(),
        execution_status: execution.status.clone(),
        final_message,
        delivery,
        wall_ms: measured_wall_ms,
        execution_wall_ms: execution.wall_ms,
        tokens,
        token_coverage: execution.coverage.clone(),
        tokens_complete,
        validation,
        error,
        ranking_score: None,
    };
    Ok((result, Some(execution)))
}

fn execution_tokens(execution: &Execution) -> Option<u64> {
    execution
        .metrics
        .as_ref()
        .filter(|metrics| metrics.tokens.valid() && metrics.total_tokens == metrics.tokens.total())
        .map(|metrics| metrics.total_tokens)
        .or_else(|| {
            execution
                .reported_tokens
                .filter(|tokens| tokens.valid())
                .map(|tokens| tokens.total())
        })
}

fn validate(cwd: &Path, command: &str, timeout_secs: u64) -> Result<ValidationResult> {
    let stdout_file = tempfile::tempfile()?;
    let stderr_file = tempfile::tempfile()?;
    let started = Instant::now();
    #[cfg(unix)]
    let mut child = {
        use std::os::unix::process::CommandExt;
        let mut child = Command::new("sh");
        child
            .arg("-c")
            .arg(command)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(stdout_file.try_clone()?)
            .stderr(stderr_file.try_clone()?)
            .process_group(0);
        child
            .spawn()
            .context("Não foi possível iniciar a validação")?
    };
    #[cfg(windows)]
    let mut child = Command::new("cmd")
        .arg("/C")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(stdout_file.try_clone()?)
        .stderr(stderr_file.try_clone()?)
        .spawn()
        .context("Não foi possível iniciar a validação")?;

    let deadline = Duration::from_secs(timeout_secs);
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait()? {
            break (status, false);
        }
        if started.elapsed() >= deadline {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            #[cfg(windows)]
            child.kill()?;
            break (child.wait()?, true);
        }
        thread::sleep(Duration::from_millis(20));
    };
    use std::io::Seek;
    let mut stdout_file = stdout_file;
    let mut stderr_file = stderr_file;
    stdout_file.rewind()?;
    stderr_file.rewind()?;
    let stdout = read_limited(&mut stdout_file)?;
    let stderr = read_limited(&mut stderr_file)?;
    Ok(ValidationResult {
        command: command.into(),
        success: status.success() && !timed_out,
        exit_code: status.code(),
        timed_out,
        wall_ms: started.elapsed().as_millis() as u64,
        stdout,
        stderr,
    })
}

fn read_limited(file: &mut fs::File) -> Result<String> {
    use std::io::Read;
    let mut bytes = Vec::with_capacity(OUTPUT_LIMIT + 1);
    file.take((OUTPUT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() > OUTPUT_LIMIT;
    bytes.truncate(OUTPUT_LIMIT);
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        text.push_str("\n… saída truncada");
    }
    Ok(text)
}

fn rank(results: &mut [ProfileResult]) -> Option<String> {
    let eligible: Vec<usize> = results
        .iter()
        .enumerate()
        .filter(|(_, result)| result.delivery == Delivery::Validated)
        .map(|(index, _)| index)
        .collect();
    if eligible.is_empty() {
        return None;
    }
    if eligible
        .iter()
        .any(|&index| results[index].tokens.is_none() || !results[index].tokens_complete)
    {
        return None;
    }
    if eligible.len() == 1 {
        results[eligible[0]].ranking_score = Some(1.0);
        return Some(results[eligible[0]].profile_name.clone());
    }
    let min_time = eligible
        .iter()
        .map(|&index| results[index].wall_ms.max(1))
        .min()? as f64;
    let min_tokens = eligible
        .iter()
        .filter_map(|&index| results[index].tokens)
        .map(|tokens| tokens.max(1))
        .min()? as f64;
    for &index in &eligible {
        let time = results[index].wall_ms.max(1) as f64;
        let tokens = results[index].tokens?.max(1) as f64;
        results[index].ranking_score = Some((min_time / time + min_tokens / tokens) / 2.0);
    }
    eligible
        .into_iter()
        .max_by(|&left, &right| {
            results[left]
                .ranking_score
                .partial_cmp(&results[right].ranking_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| results[right].profile_name.cmp(&results[left].profile_name))
        })
        .map(|index| results[index].profile_name.clone())
}

fn failed_result(profile_name: String, workspace: PathBuf, error: String) -> ProfileResult {
    ProfileResult {
        profile_name,
        workspace,
        execution_id: None,
        cli_session_id: None,
        execution_status: "failed".into(),
        final_message: String::new(),
        delivery: Delivery::Inconclusive,
        wall_ms: 0,
        execution_wall_ms: None,
        tokens: None,
        token_coverage: "unavailable".into(),
        tokens_complete: false,
        validation: None,
        error: Some(error),
        ranking_score: None,
    }
}

fn reports_dir(cwd: &Path) -> PathBuf {
    cwd.join(".stackpulse").join("comparisons")
}

fn persist(cwd: &Path, comparison_dir: &Path, report: &ComparisonReport) -> Result<()> {
    let directory = reports_dir(cwd);
    fs::create_dir_all(&directory)?;
    let bytes = serde_json::to_vec_pretty(report)?;
    profiles::atomic_write(&comparison_dir.join("report.json"), &bytes)?;
    profiles::atomic_write(&directory.join("latest.json"), &bytes)?;
    Ok(())
}

fn copy_workspace(source: &Path, destination: &Path) -> Result<()> {
    let canonical_source = source.canonicalize()?;
    for entry in walkdir::WalkDir::new(source)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || !matches!(
                    entry.file_name().to_str(),
                    Some(".git" | ".stackpulse" | "target")
                )
        })
    {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        let target = destination.join(relative);
        let kind = entry.file_type();
        if kind.is_dir() {
            fs::create_dir_all(&target)?;
        } else if kind.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &target)?;
            fs::set_permissions(&target, entry.metadata()?.permissions())?;
        } else if kind.is_symlink() {
            copy_symlink(entry.path(), &target, &canonical_source, destination)?;
        } else {
            bail!(
                "Tipo de arquivo não suportado no workspace: {}",
                entry.path().display()
            );
        }
    }
    Ok(())
}

#[cfg(unix)]
fn copy_symlink(link: &Path, target: &Path, source: &Path, destination: &Path) -> Result<()> {
    use std::os::unix::fs::symlink;
    let resolved = link
        .canonicalize()
        .with_context(|| format!("Link simbólico inválido: {}", link.display()))?;
    ensure!(
        resolved.starts_with(source),
        "Link simbólico aponta para fora do workspace: {}",
        link.display()
    );
    let copied_target = destination.join(resolved.strip_prefix(source)?);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    symlink(copied_target, target)?;
    Ok(())
}

#[cfg(windows)]
fn copy_symlink(link: &Path, target: &Path, source: &Path, destination: &Path) -> Result<()> {
    use std::os::windows::fs::{symlink_dir, symlink_file};
    let resolved = link.canonicalize()?;
    ensure!(
        resolved.starts_with(source),
        "Link simbólico aponta para fora do workspace"
    );
    let copied_target = destination.join(resolved.strip_prefix(source)?);
    if resolved.is_dir() {
        symlink_dir(copied_target, target)?;
    } else {
        symlink_file(copied_target, target)?;
    }
    Ok(())
}
