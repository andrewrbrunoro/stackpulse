//! Interactive profile comparison: selection precedes request composition.
use crate::{
    profiles::Discovered,
    tui::{self, Frame, TerminalGuard, Tone},
    workflow,
};
use anyhow::{Result, bail, ensure};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal,
};
use std::io::{self, IsTerminal};

#[derive(Default)]
struct Selection {
    cursor: usize,
    checked: Vec<bool>,
}
impl Selection {
    fn new(count: usize) -> Self {
        Self {
            cursor: 0,
            checked: vec![false; count],
        }
    }
    fn toggle(&mut self) {
        self.checked[self.cursor] = !self.checked[self.cursor];
    }
    fn names(&self, profiles: &[Discovered]) -> Result<Vec<String>> {
        let names: Vec<_> = profiles
            .iter()
            .zip(&self.checked)
            .filter(|(_, checked)| **checked)
            .map(|(p, _)| p.name.clone())
            .collect();
        ensure!(
            names.len() >= 2,
            "Selecione ao menos 2 perfis para comparar."
        );
        Ok(names)
    }
}

pub fn select(profiles: &[Discovered]) -> Result<Vec<String>> {
    ensure!(
        profiles.len() >= 2,
        "A comparação exige ao menos 2 perfis. Crie novos perfis com stackpulse profiles add."
    );
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "Sem terminal interativo, informe ao menos dois --profile e --prompt."
    );
    let mut selection = Selection::new(profiles.len());
    if !tui::available() {
        loop {
            println!("\nSTACKPULSE · Comparar perfis");
            for (i, profile) in profiles.iter().enumerate() {
                println!(
                    "{} [{}] {}",
                    i + 1,
                    if selection.checked[i] { "x" } else { " " },
                    profile.name
                );
            }
            let answer = workflow::ask("Número alterna; Enter continua; q cancela", "")?;
            if answer == "q" {
                bail!("Comparação cancelada");
            }
            if answer.is_empty() {
                match selection.names(profiles) {
                    Ok(names) => return Ok(names),
                    Err(e) => eprintln!("{e}"),
                }
            } else if let Ok(number) = answer.parse::<usize>() {
                if (1..=profiles.len()).contains(&number) {
                    selection.cursor = number - 1;
                    selection.toggle();
                }
            }
        }
    }
    let _guard = TerminalGuard::enter()?;
    let mut notice = String::new();
    loop {
        let (columns, rows) = terminal::size()?;
        let mut frame = Frame::new(columns as usize, rows as usize);
        if rows < 10 || columns < 45 {
            frame.put(
                1,
                1,
                "Amplie o terminal para 45×10. Esc cancela.",
                Tone::Warning,
            );
        } else {
            frame.put(2, 1, "STACKPULSE · Comparar perfis", Tone::Accent);
            frame.put(
                2,
                3,
                "↑/↓ mover · Espaço marcar · Enter continuar",
                Tone::Muted,
            );
            let capacity = (rows as usize).saturating_sub(8).max(1);
            let first = selection.cursor.saturating_sub(capacity - 1);
            for (index, profile) in profiles.iter().enumerate().skip(first).take(capacity) {
                frame.put(
                    2,
                    5 + index - first,
                    &format!(
                        "{} [{}] {}",
                        if index == selection.cursor {
                            "❯"
                        } else {
                            " "
                        },
                        if selection.checked[index] { "x" } else { " " },
                        profile.name
                    ),
                    if index == selection.cursor {
                        Tone::Selected
                    } else {
                        Tone::Text
                    },
                );
            }
            frame.put(2, rows as usize - 2, &notice, Tone::Warning);
        }
        frame.draw(columns, rows, tui::colors())?;
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Release {
                continue;
            }
            if key.code == KeyCode::Esc
                || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
            {
                bail!("Comparação cancelada");
            }
            if rows < 10 || columns < 45 {
                continue;
            }
            match key.code {
                KeyCode::Up => selection.cursor = selection.cursor.saturating_sub(1),
                KeyCode::Down | KeyCode::Tab => {
                    selection.cursor = (selection.cursor + 1) % profiles.len()
                }
                KeyCode::Char(' ') => {
                    selection.toggle();
                    notice.clear();
                }
                KeyCode::Enter => match selection.names(profiles) {
                    Ok(names) => return Ok(names),
                    Err(e) => notice = e.to_string(),
                },
                _ => {}
            }
        }
    }
}

pub fn request(
    prompt: Option<String>,
    validation: Option<String>,
) -> Result<(String, Option<String>)> {
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    let prompt = match prompt {
        Some(prompt) => prompt,
        None => {
            ensure!(
                interactive,
                "Informe --prompt para comparar sem terminal interativo"
            );
            workflow::ask("Prompt/pedido para todos os perfis", "")?
        }
    };
    ensure!(!prompt.trim().is_empty(), "O pedido não pode estar vazio");
    let validation = if validation.is_none() && interactive {
        println!(
            "O mesmo teste será executado em cada entrega. Sem teste, o resultado fica inconclusivo e sem troféu."
        );
        let value = workflow::ask("Comando para validar a entrega (ex.: cargo test)", "")?;
        (!value.is_empty()).then_some(value)
    } else {
        validation
    };
    Ok((prompt, validation))
}

/// Obtain consent before comparison compilation, snapshots, or workers write to tmp.
pub fn authorize_tmp(allowed: bool) -> Result<()> {
    if allowed {
        return Ok(());
    }
    let directory = std::env::temp_dir().canonicalize()?;
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "A comparação precisa de permissão para escrever em {}. Use --allow-tmp para autorizar sem interação.",
        directory.display()
    );
    let answer = workflow::ask(
        &format!(
            "Permitir escrever em {} e manter os arquivos em stackpulse-{{data-hora}}-compare/ (s/n)?",
            directory.display()
        ),
        "n",
    )?;
    ensure!(
        matches!(answer.to_lowercase().as_str(), "s" | "sim" | "y" | "yes"),
        "Comparação cancelada: escrita na pasta temporária não autorizada"
    );
    Ok(())
}

pub fn summary(report: &crate::compare::ComparisonReport, cwd: &std::path::Path) {
    println!("\nSTACKPULSE · Resultado da comparação");
    println!(
        "{:<28} {:<14} {:>12} {:>14}",
        "Perfil", "Entrega", "Tempo", "Tokens"
    );
    for result in &report.results {
        let delivery = match result.delivery {
            crate::compare::Delivery::Validated => "aprovada",
            crate::compare::Delivery::Rejected => "reprovada",
            crate::compare::Delivery::Inconclusive => "inconclusiva",
        };
        let name = format!(
            "{}{}",
            result.profile_name,
            if report.winner.as_deref() == Some(&result.profile_name) {
                " 🏆"
            } else {
                ""
            }
        );
        println!(
            "{:<28} {:<14} {:>12} {:>14}",
            crate::widget::clean(&name, 28),
            delivery,
            crate::widget::duration(result.wall_ms as i64),
            result
                .tokens
                .map(|n| format!(
                    "{n}{}",
                    if result.tokens_complete {
                        ""
                    } else {
                        " (parcial)"
                    }
                ))
                .unwrap_or_else(|| "indisponíveis".into())
        );
        if let Some(error) = &result.error {
            println!("  {}", crate::widget::clean(error, 160));
        }
    }
    match &report.winner {
        Some(name) => println!("\n🏆 {name} · vencedor desta comparação"),
        None => println!(
            "\nSem vencedor: consulte a validação e a cobertura das métricas no relatório."
        ),
    }
    let directory = if report.output_dir.as_os_str().is_empty() {
        cwd.join(".stackpulse/comparisons").join(&report.id)
    } else {
        report.output_dir.clone()
    };
    println!("Arquivos da comparação: {}", directory.display());
    println!("Relatório: {}", directory.join("report.json").display());
    for result in &report.results {
        println!("  {}: {}", result.profile_name, result.workspace.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_requires_two_and_preserves_names() {
        let profiles: Vec<_> = ["alpha", "beta", "gamma"]
            .into_iter()
            .map(|name| Discovered {
                name: name.into(),
                path: name.into(),
                scope: "all".into(),
                compiled: true,
            })
            .collect();
        let mut selection = Selection::new(3);
        assert!(selection.names(&profiles).is_err());
        selection.toggle();
        assert!(selection.names(&profiles).is_err());
        selection.cursor = 2;
        selection.toggle();
        assert_eq!(selection.names(&profiles).unwrap(), vec!["alpha", "gamma"]);
        selection.toggle();
        assert!(selection.names(&profiles).is_err());
    }
}
