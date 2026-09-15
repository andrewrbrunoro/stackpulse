//! One selection for all optional integrations, shared with the shell installer.
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    io::{self, IsTerminal, Write},
    path::Path,
};

#[derive(Debug, PartialEq)]
pub struct Choices {
    pub memory: bool,
    pub usagebar: bool,
}

fn choices(
    memory: Option<bool>,
    usagebar: Option<bool>,
    selected: Option<(bool, bool)>,
) -> Choices {
    let selected = selected.unwrap_or((false, false));
    Choices {
        memory: memory.unwrap_or(selected.0),
        usagebar: crate::usagebar::supported() && usagebar.unwrap_or(selected.1),
    }
}

/// None means an editable default; Some is a choice already made through a flag.
pub fn select(memory: Option<bool>, usagebar: Option<bool>) -> Result<Choices> {
    let supported = crate::usagebar::supported();
    ensure!(
        usagebar != Some(true) || supported,
        crate::usagebar::platform_description()
    );
    if memory.is_some() && (usagebar.is_some() || !supported) {
        return Ok(choices(memory, usagebar, None));
    }
    if crate::tui::available() {
        return Ok(choices(
            memory,
            usagebar,
            crate::memory_ui::select_plugins(memory, usagebar)?,
        ));
    }
    let mut selected = (
        memory.unwrap_or(true),
        supported && usagebar.unwrap_or(false),
    );
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    loop {
        println!("\nSTACKPULSE · PLUGINS OPCIONAIS");
        println!(
            "1. [{}] AI-Memory{}",
            if selected.0 { "x" } else { " " },
            fixed_label(memory)
        );
        if supported {
            println!(
                "2. [{}] AI-UsageBar{}",
                if selected.1 { "x" } else { " " },
                fixed_label(usagebar)
            );
        }
        println!("AI-Memory: memória de longo prazo; conexão aos agentes em etapa separada.");
        println!("{}", crate::memory::PROJECT_URL);
        if supported {
            println!("AI-UsageBar: CLI/TUI; barra gráfica configurada separadamente.");
            println!("{}", crate::usagebar::PROJECT_URL);
            println!("{}", crate::usagebar::platform_description());
        }
        println!(
            "Criados por Fabio Akita (AkitaOnRails) · {}",
            crate::memory::CREATOR_URL
        );
        if !interactive {
            return Ok(choices(memory, usagebar, Some(selected)));
        }
        print!("Número alterna · Enter confirma · q dispensa editáveis: ");
        io::stdout().flush()?;
        let mut value = String::new();
        ensure!(
            io::stdin().read_line(&mut value)? > 0,
            "Seleção interrompida sem confirmação."
        );
        match value.trim() {
            "" => return Ok(choices(memory, usagebar, Some(selected))),
            "q" | "Q" => return Ok(choices(memory, usagebar, None)),
            "1" if memory.is_none() => selected.0 = !selected.0,
            "2" if supported && usagebar.is_none() => selected.1 = !selected.1,
            _ => println!("Escolha um plugin editável ou confirme com Enter."),
        }
    }
}

fn fixed_label(mode: Option<bool>) -> &'static str {
    if mode.is_some() {
        " · definido pela flag"
    } else {
        ""
    }
}

/// Keep stdout attached to the terminal; exchange choices through a fresh file.
pub fn select_to_file(memory: Option<bool>, usagebar: Option<bool>, path: &Path) -> Result<()> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    match fs::symlink_metadata(&absolute) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).context("Não foi possível verificar o arquivo de seleção.");
        }
        Ok(_) => anyhow::bail!("Arquivo de seleção já existe; conteúdo preservado."),
    }
    let parent = absolute.parent().context("Destino da seleção inválido.")?;
    // Check that the installer can receive the result before asking the user.
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    let selected = select(memory, usagebar)?;
    writeln!(
        temporary,
        "memory={}",
        if selected.memory { "with" } else { "without" }
    )?;
    writeln!(
        temporary,
        "usagebar={}",
        if selected.usagebar { "with" } else { "without" }
    )?;
    temporary
        .persist_noclobber(&absolute)
        .context("Não foi possível salvar a seleção; arquivos existentes preservados.")?;
    Ok(())
}
