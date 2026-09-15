//! Local executable discovery. Never starts a provider process or checks credentials.
use crate::client::Backend;
use serde::Serialize;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

struct Provider {
    id: &'static str,
    name: &'static str,
    command: &'static str,
}

// Product order, independent of installation order in PATH.
const CATALOG: &[Provider] = &[
    Provider {
        id: "codex",
        name: "Codex CLI",
        command: "codex",
    },
    Provider {
        id: "claude",
        name: "Claude Code",
        command: "claude",
    },
    Provider {
        id: "gemini",
        name: "Gemini CLI",
        command: "gemini",
    },
    Provider {
        id: "copilot",
        name: "GitHub Copilot",
        command: "copilot",
    },
    Provider {
        id: "cursor",
        name: "Cursor Agent",
        command: "cursor-agent",
    },
    Provider {
        id: "grok",
        name: "Grok CLI",
        command: "grok",
    },
    Provider {
        id: "opencode",
        name: "OpenCode",
        command: "opencode",
    },
    Provider {
        id: "aider",
        name: "Aider",
        command: "aider",
    },
    Provider {
        id: "amp",
        name: "Amp",
        command: "amp",
    },
    Provider {
        id: "droid",
        name: "Factory Droid",
        command: "droid",
    },
    Provider {
        id: "goose",
        name: "Goose",
        command: "goose",
    },
    Provider {
        id: "kiro",
        name: "Kiro CLI",
        command: "kiro-cli",
    },
];

#[derive(Debug, Clone, Serialize)]
pub struct InstalledCli {
    pub id: String,
    pub name: String,
    /// Keep the invocation path: a symlink may select behavior by argv[0].
    pub executable: PathBuf,
    pub other_installations: Vec<PathBuf>,
    pub adapter_available: bool,
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Inventory {
    pub installed: Vec<InstalledCli>,
    pub searched_directories: Vec<PathBuf>,
    pub catalog: Vec<&'static str>,
}

pub fn search_directories() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        for dir in [
            ".local/bin",
            ".npm-global/bin",
            ".bun/bin",
            ".cargo/bin",
            ".cursor/bin",
            ".grok/bin",
            ".opencode/bin",
            ".amp/bin",
            ".factory/bin",
        ] {
            dirs.push(home.join(dir));
        }
    }
    dirs.extend([
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ]);
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut seen = HashSet::new();
    dirs.into_iter()
        .map(|dir| {
            if dir.is_absolute() {
                dir
            } else {
                cwd.join(dir)
            }
        })
        .filter(|dir| seen.insert(dir.clone()))
        .collect()
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub fn same_executable(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

pub fn resolve_executable(path: &Path, dirs: &[PathBuf]) -> Option<PathBuf> {
    if path.is_absolute() || path.components().count() > 1 {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir().ok()?.join(path)
        };
        return is_executable(&absolute).then_some(absolute);
    }
    dirs.iter()
        .map(|dir| dir.join(path))
        .find(|path| is_executable(path))
}

fn candidates(command: &str, dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    dirs.iter()
        .map(|dir| dir.join(command))
        .filter(|path| is_executable(path))
        .filter(|path| seen.insert(path.canonicalize().unwrap_or_else(|_| path.clone())))
        .collect()
}

fn alias_provider(path: &Path) -> Option<&'static str> {
    let target = path.canonicalize().ok()?;
    let parts: Vec<_> = target
        .components()
        .map(|part| part.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    if parts
        .iter()
        .any(|part| part == "cursor-agent" || part.starts_with("cursor-agent-"))
    {
        Some("cursor")
    } else if parts
        .iter()
        .any(|part| part == ".grok" || part == "grok" || part.starts_with("grok-"))
    {
        Some("grok")
    } else {
        None
    }
}

pub fn discover(dirs: &[PathBuf]) -> Inventory {
    let aliases = candidates("agent", dirs);
    let mut installed = Vec::new();
    for provider in CATALOG {
        let mut paths = candidates(provider.command, dirs);
        for alias in &aliases {
            if alias_provider(alias) == Some(provider.id)
                && !paths.iter().any(|p| same_executable(p, alias))
            {
                paths.push(alias.clone());
            }
        }
        if paths.is_empty() {
            continue;
        }
        let executable = paths.remove(0);
        // AWS also distributes a `copilot` command. Do not assert a vendor from that name.
        let ambiguous_copilot = provider.id == "copilot"
            && !executable
                .canonicalize()
                .unwrap_or_else(|_| executable.clone())
                .to_string_lossy()
                .contains("@github/copilot");
        installed.push(InstalledCli {
            id: provider.id.into(),
            name: if ambiguous_copilot {
                "Copilot (origem a confirmar)"
            } else {
                provider.name
            }
            .into(),
            executable,
            other_installations: paths,
            adapter_available: Backend::from_cli_id(provider.id).is_some(),
            note: ambiguous_copilot
                .then(|| "O nome copilot também é usado pelo CLI da AWS.".into()),
        });
    }
    // An unrecognized `agent` must never silently become Cursor.
    for alias in aliases {
        if installed.iter().any(|cli| {
            std::iter::once(&cli.executable)
                .chain(&cli.other_installations)
                .any(|path| same_executable(path, &alias))
        }) {
            continue;
        }
        installed.push(InstalledCli {
            id: "agent-unknown".into(),
            name: "Agent (origem a confirmar)".into(),
            executable: alias,
            other_installations: Vec::new(),
            adapter_available: false,
            note: Some("Alias genérico; não foi atribuído a um provider.".into()),
        });
    }
    Inventory {
        installed,
        searched_directories: dirs.to_vec(),
        catalog: CATALOG.iter().map(|p| p.name).collect(),
    }
}

pub fn print_inventory(inventory: &Inventory) {
    println!("CLIs instalados · principais primeiro");
    if inventory.installed.is_empty() {
        println!("  Nenhum CLI do catálogo encontrado.");
    }
    for (i, cli) in inventory.installed.iter().enumerate() {
        println!(
            "  {}. {} · {}\n     {}",
            i + 1,
            cli.name,
            if cli.adapter_available {
                "integração disponível"
            } else {
                "instalado · sem adaptador no Timeline"
            },
            cli.executable.display()
        );
        for path in &cli.other_installations {
            println!("     Outra instalação: {}", path.display());
        }
        if let Some(note) = &cli.note {
            println!("     {note}");
        }
    }
    println!(
        "Busca local no PATH e pastas comuns ({} CLIs no catálogo). O login existente do CLI será reutilizado.\n",
        inventory.catalog.len()
    );
}
