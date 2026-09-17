use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub schema_version: u32,
    #[serde(default)]
    pub client: crate::client::Backend,
    pub provider: String,
    pub model: String,
    pub effort: String,
    pub executable: PathBuf,
    pub providers_root: PathBuf,
    pub default_profile: Option<String>,
    pub max_agents: u32,
    /// Install the optional memory package during setup; this is not connection status.
    #[serde(default = "default_ai_memory")]
    pub ai_memory: bool,
    /// Install AI-UsageBar on supported platforms; independent of its graphical setup.
    #[serde(default)]
    pub ai_usagebar: bool,
}

fn default_ai_memory() -> bool {
    true
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.schema_version == 1, "Versão de setup não suportada");
        identifier(&self.provider)?;
        model_id(&self.model)?;
        effort(&self.effort)?;
        ensure!(
            (1..=100).contains(&self.max_agents),
            "max-agents deve estar entre 1 e 100"
        );
        ensure!(
            self.providers_root.is_absolute(),
            "providers_root deve ser absoluto"
        );
        if let Some(name) = &self.default_profile {
            identifier(name)?;
        }
        Ok(())
    }
    pub fn read(path: &Path) -> Result<Self> {
        let value: Self = serde_json::from_slice(&fs::read(path).with_context(|| {
            format!(
                "Execute setup primeiro; configuração não encontrada: {}",
                path.display()
            )
        })?)?;
        value.validate()?;
        Ok(value)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        atomic_write(path, serde_json::to_vec_pretty(self)?.as_slice())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub role: String,
    pub model: String,
    pub effort: String,
    pub purpose: String,
    pub when: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamSpec {
    pub name: String,
    pub provider: String,
    pub orchestrator: AgentSpec,
    pub agents: Vec<AgentSpec>,
    pub delegation: String,
    pub integration: String,
    pub notes: String,
}

impl TeamSpec {
    pub fn provider_for<'a>(&'a self, agent: &'a AgentSpec) -> &'a str {
        agent.provider.as_deref().unwrap_or(&self.provider)
    }

    pub fn root_provider(&self) -> &str {
        self.provider_for(&self.orchestrator)
    }

    pub fn is_mixed(&self) -> bool {
        let root = self.root_provider();
        self.agents.iter().any(|agent| {
            let provider = self.provider_for(agent);
            provider != root
                && match (
                    crate::client::Backend::for_provider(root),
                    crate::client::Backend::for_provider(provider),
                ) {
                    (Some(root), Some(child)) => root != child,
                    _ => true,
                }
        })
    }

    /// Runtime compatibility without probing installed CLIs or authenticating.
    pub fn validate_execution_providers(&self) -> Result<()> {
        if !self.is_mixed() {
            return Ok(());
        }
        use crate::client::Backend;
        ensure!(
            matches!(
                Backend::for_provider(self.root_provider()),
                Some(Backend::Codex | Backend::Claude)
            ),
            "Equipes com múltiplos providers exigem root Codex/Claude; provider recebido: {}",
            self.root_provider()
        );
        for agent in &self.agents {
            ensure!(
                Backend::for_provider(self.provider_for(agent)) != Some(Backend::Cursor),
                "O papel {} usa Cursor, cujo adaptador ainda não desativa subagentes nativos; use Codex, Claude ou Grok na ponte.",
                agent.role
            );
            ensure!(
                Backend::for_provider(self.provider_for(agent)).is_some(),
                "Nenhum adaptador para o provider {} do papel {}",
                self.provider_for(agent),
                agent.role
            );
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        identifier(&self.name)?;
        identifier(&self.provider)?;
        ensure!(self.agents.len() <= 32, "No máximo 32 papéis por perfil");
        let mut roles = std::collections::HashSet::new();
        for a in std::iter::once(&self.orchestrator).chain(&self.agents) {
            identifier(&a.role)?;
            if let Some(provider) = &a.provider {
                identifier(provider)?;
            }
            model_id(&a.model)?;
            if a.effort != "unknown" {
                effort(&a.effort)?;
            }
            ensure!(roles.insert(&a.role), "Papel duplicado: {}", a.role);
            ensure!(
                !a.purpose.trim().is_empty() && a.purpose.len() <= 4000 && a.when.len() <= 2000,
                "Descrição do papel inválida"
            );
        }
        ensure!(
            matches!(
                self.delegation.as_str(),
                "on_demand" | "parallel" | "sequential"
            ),
            "Delegação inválida"
        );
        ensure!(
            self.integration.len() <= 4000 && self.notes.len() <= 8000,
            "Descrição muito longa"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub schema_version: u32,
    pub source_image: String,
    pub source_sha256: String,
    pub generated_by: String,
    pub generated_at: DateTime<Utc>,
    pub team: TeamSpec,
}

pub fn identifier(s: &str) -> Result<()> {
    ensure!(
        !s.is_empty()
            && s.len() <= 100
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)),
        "Use identificadores com letras, números, hífen ou sublinhado: {s}"
    );
    Ok(())
}
pub fn scope_name(s: &str) -> Result<()> {
    ensure!(
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.len() <= 200
            && !s.contains(['/', '\\'])
            && !s.chars().any(char::is_control),
        "Nome de projeto inválido"
    );
    Ok(())
}
fn model_id(s: &str) -> Result<()> {
    ensure!(
        !s.is_empty() && s.len() <= 200 && s.chars().all(|c| !c.is_control() && !c.is_whitespace()),
        "Modelo inválido"
    );
    Ok(())
}
fn effort(s: &str) -> Result<()> {
    ensure!(
        matches!(
            s,
            "default" | "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
        ),
        "Esforço inválido: {s}"
    );
    Ok(())
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

pub fn markdown(profile: &Profile) -> Result<String> {
    profile.team.validate()?;
    let t = &profile.team;
    let mut s = format!(
        "+++\n{}+++\n\n# {}\n\nPerfil extraído de `{}`. A configuração executável está no bloco TOML acima.\n\n| Papel | Provider | Modelo | Esforço | Quando |\n|---|---|---|---|---|\n",
        toml::to_string_pretty(profile)?,
        t.name,
        profile.source_image
    );
    for a in std::iter::once(&t.orchestrator).chain(&t.agents) {
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            a.role,
            t.provider_for(a),
            a.model,
            a.effort,
            a.when.replace('|', "/").replace('\n', " ")
        ));
    }
    s.push_str(&format!(
        "\nDelegação: `{}`.\n\nIntegração: {}\n\n{}\n",
        t.delegation, t.integration, t.notes
    ));
    Ok(s)
}

pub fn read(path: &Path) -> Result<(Profile, String)> {
    let content = fs::read_to_string(path)?;
    ensure!(content.len() <= 100_000, "Perfil maior que 100 KB");
    let normalized = content.replace("\r\n", "\n");
    let front = normalized
        .strip_prefix("+++\n")
        .and_then(|s| s.split_once("\n+++\n"))
        .context("Perfil precisa de frontmatter TOML entre +++")?
        .0;
    let profile: Profile = toml::from_str(front)?;
    ensure!(
        profile.schema_version == 1,
        "Versão de perfil não suportada"
    );
    profile.team.validate()?;
    Ok((profile, content))
}

pub fn project_root(cwd: &Path) -> PathBuf {
    // No shell and no project hooks; rev-parse is a read-only query.
    if let Ok(out) = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        && out.status.success()
    {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() {
            return PathBuf::from(s);
        }
    }
    cwd.to_path_buf()
}

pub fn project_key(cwd: &Path) -> Result<String> {
    let root = project_root(cwd);
    let name = root
        .file_name()
        .and_then(|s| s.to_str())
        .context("Projeto sem nome")?;
    scope_name(name)?;
    Ok(name.into())
}

#[derive(Debug, Clone, Serialize)]
pub struct Discovered {
    pub name: String,
    pub path: PathBuf,
    pub scope: String,
    pub compiled: bool,
}

pub fn discover(settings: &Settings, cwd: &Path) -> Result<Vec<Discovered>> {
    let key = project_key(cwd)?;
    let mut found = BTreeMap::<String, Discovered>::new();
    // Project-specific entries override an all entry of the same name, including pending images.
    for scope in ["all", key.as_str()] {
        let dir = settings.providers_root.join("project").join(scope);
        if !dir.exists() {
            continue;
        }
        for item in fs::read_dir(dir)? {
            let path = item?.path();
            if !path.is_file() {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !matches!(ext.as_str(), "md" | "png" | "jpg" | "jpeg" | "webp") {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .context("Nome inválido")?
                .to_string();
            identifier(&name)?;
            let compiled = ext == "md";
            if found
                .get(&name)
                .is_some_and(|d| d.scope == scope && d.compiled && !compiled)
            {
                continue;
            }
            found.insert(
                name.clone(),
                Discovered {
                    name,
                    path,
                    scope: scope.into(),
                    compiled,
                },
            );
        }
    }
    Ok(found.into_values().collect())
}

pub fn resolve(settings: &Settings, cwd: &Path, name: Option<&str>) -> Result<Discovered> {
    let all = discover(settings, cwd)?;
    let requested = name.or(settings.default_profile.as_deref());
    if let Some(name) = requested {
        identifier(name)?;
        return all
            .into_iter()
            .find(|d| d.name == name)
            .with_context(|| format!("Perfil {name} não encontrado; use profiles list"));
    }
    ensure!(
        all.len() == 1,
        "{} perfis encontrados; escolha com --profile ou setup --default-profile",
        all.len()
    );
    Ok(all.into_iter().next().unwrap())
}

pub fn check_image(profile: &Profile, md: &Path) -> Result<()> {
    let name = Path::new(&profile.source_image);
    ensure!(
        name.file_name() == Some(name.as_os_str()),
        "source_image precisa ser apenas o nome do arquivo"
    );
    let image = md.parent().unwrap().join(name);
    ensure!(
        image.exists(),
        "Imagem de origem ausente: {}",
        image.display()
    );
    ensure!(
        digest(&fs::read(image)?) == profile.source_sha256,
        "Imagem mudou: rode profiles compile novamente"
    );
    Ok(())
}

pub fn extraction_schema() -> serde_json::Value {
    let agent = serde_json::json!({"type":"object","additionalProperties":false,"properties":{
        "provider":{"type":["string","null"],"pattern":"^[a-zA-Z0-9_-]+$"},"role":{"type":"string","pattern":"^[a-zA-Z0-9_-]+$"},"model":{"type":"string"},"effort":{"type":"string","enum":["unknown","none","minimal","low","medium","high","xhigh","max","ultra"]},"purpose":{"type":"string"},"when":{"type":"string"}},"required":["provider","role","model","effort","purpose","when"]});
    serde_json::json!({"type":"object","additionalProperties":false,"properties":{
        "name":{"type":"string"},"provider":{"type":"string","pattern":"^[a-zA-Z0-9_-]+$"},"orchestrator":agent,
        "agents":{"type":"array","items":agent},"delegation":{"type":"string","enum":["on_demand","parallel","sequential"]},
        "integration":{"type":"string"},"notes":{"type":"string"}},"required":["name","provider","orchestrator","agents","delegation","integration","notes"]})
}

pub fn load_for_run(
    settings: &Settings,
    cwd: &Path,
    name: Option<&str>,
) -> Result<(Discovered, Profile, String)> {
    let d = resolve(settings, cwd, name)?;
    if !d.compiled {
        bail!(
            "Imagem encontrada: {}. Rode profiles compile {} primeiro",
            d.path.display(),
            d.path.display()
        );
    }
    let (profile, md) = read(&d.path)?;
    ensure!(
        std::iter::once(&profile.team.orchestrator)
            .chain(&profile.team.agents)
            .all(|a| profile.team.provider_for(a) != "unknown"
                && a.model != "unknown"
                && a.effort != "unknown"),
        "Perfil contém campos unknown; complete o TOML do Markdown antes de executar"
    );
    profile.team.validate_execution_providers()?;
    check_image(&profile, &d.path)?;
    Ok((d, profile, md))
}
