use crate::{
    analytics::{self, Period, Report, Scope},
    collector::{self, ImportReport},
    db::Db,
    trend,
    workflow::Execution,
};
use anyhow::{Result, ensure};
use chrono::Utc;
use chrono_tz::Tz;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Attribute, Color, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{self, ClearType},
};
use std::{
    io::{self, IsTerminal, Write},
    path::Path,
    time::{Duration, Instant},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub fn number(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.2}M", value as f64 / 1_000_000.0)
    } else if value >= 1000 {
        format!("{:.1}k", value as f64 / 1000.0)
    } else {
        value.to_string()
    }
}

pub fn duration(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    if seconds >= 3600 {
        format!("{}h{:02}m", seconds / 3600, (seconds % 3600) / 60)
    } else if seconds >= 60 {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

pub fn clean(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars().filter(|c| !c.is_control()) {
        let w = c.width().unwrap_or(0);
        if used + w > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out
}

pub fn one_line(report: &Report) -> String {
    let m = &report.current;
    let cost = m
        .estimated_usd
        .map_or_else(|| "$?".into(), |v| format!("~${v:.2}"));
    format!(
        "AI {} · {} tok · {} · {} · {} agentes",
        report.period.label(),
        number(m.total_tokens),
        cost,
        duration(m.active_ms),
        m.agents
    )
}

fn pair(left: &str, right: &str, width: usize) -> String {
    let left = clean(left, width);
    let right = clean(right, width);
    let gap = width.saturating_sub(left.width() + right.width());
    if gap > 0 {
        format!("{left}{}{right}", " ".repeat(gap))
    } else {
        format!("{left}  {right}")
    }
}

fn frame(text: &str, inner: usize) -> String {
    let text = clean(text, inner);
    format!("│ {text}{} │", " ".repeat(inner - text.width()))
}

pub fn render(
    report: &Report,
    sync: Option<&ImportReport>,
    width: usize,
    scope: &str,
    live: bool,
) -> Vec<String> {
    let inner = width.clamp(30, 68) - 4;
    let frame = |text: &str| frame(text, inner);
    let m = &report.current;
    let top = format!("╭{}╮", "─".repeat(inner + 2));
    let bottom = format!("╰{}╯", "─".repeat(inner + 2));
    let cache = if m.tokens.input_tokens > 0 {
        100.0 * m.tokens.cached_input_tokens as f64 / m.tokens.input_tokens as f64
    } else {
        0.0
    };
    let (cost, cost_note) = m.estimated_usd.map_or_else(
        || {
            (
                "CUSTO ?".into(),
                format!("sem tarifa ({}/{})", m.priced_events, m.events),
            )
        },
        |v| (format!("CUSTO ~US$ {v:.4}"), "estimado".into()),
    );
    let trend = report.token_change_pct.map_or_else(
        || "sem base anterior".into(),
        |v| format!("{v:+.1}% vs janela anterior*"),
    );
    let max = report.series.iter().copied().max().unwrap_or(0);
    let spark: String = report
        .series
        .iter()
        .map(|v| {
            if *v == 0 {
                '·'
            } else {
                ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█']
                    [(((*v as f64 / max.max(1) as f64) * 7.0).round() as usize).min(7)]
            }
        })
        .collect();
    let status = match sync {
        Some(s) if !s.errors.is_empty() || s.malformed_lines > 0 => format!(
            "sync: {} erros / {} linhas inválidas",
            s.errors.len(),
            s.malformed_lines
        ),
        Some(s) => format!(
            "{} agentes · {} exec. · {}",
            m.agents,
            m.runs,
            s.synced_at
                .map(|t| t
                    .with_timezone(&report.timezone.parse::<Tz>().unwrap_or(chrono_tz::UTC))
                    .format("%H:%M:%S")
                    .to_string())
                .unwrap_or_default()
        ),
        None => format!("{} agentes · {} exec. · histórico local", m.agents, m.runs),
    };
    let selected = match (report.period, inner >= 44) {
        (Period::Hour, true) => "[h]ora  d:dia  m:mês",
        (Period::Day, true) => "h:hora  [d]ia  m:mês",
        (Period::Month, true) => "h:hora  d:dia  [m]ês",
        (Period::Hour, false) => "[h] d m",
        (Period::Day, false) => "h [d] m",
        (Period::Month, false) => "h d [m]",
    };
    let footer = if live {
        if inner >= 44 {
            format!("{selected}  g:entrega  q:sair")
        } else {
            format!("{selected}  g:visão  q:sair")
        }
    } else {
        "*Mesmo tempo decorrido; volume ≠ capacidade".into()
    };
    let header = if inner >= 44 {
        pair(
            "STACKPULSE",
            &format!("{} · {scope}", report.period.label()),
            inner,
        )
    } else {
        format!("STACKPULSE · {}", report.period.label())
    };
    let tokens_detail = if inner >= 44 {
        format!(
            "{} entrada · {} saída",
            number(m.tokens.input_tokens),
            number(m.tokens.output_tokens)
        )
    } else {
        format!(
            "↓{} ↑{}",
            number(m.tokens.input_tokens),
            number(m.tokens.output_tokens)
        )
    };
    let active = format!(
        "{}{}",
        if m.open_turns > 0 || m.untimed_events > 0 {
            "~"
        } else {
            ""
        },
        duration(m.active_ms)
    );
    let (active, agents) = if inner >= 44 {
        (
            format!("TEMPO {active} ativo"),
            format!("{} agentes", duration(m.agent_ms)),
        )
    } else {
        (
            format!("Ativo {active}"),
            format!("{} ag.", duration(m.agent_ms)),
        )
    };
    vec![
        top,
        frame(&header),
        frame(&pair(
            &format!("TOKENS {}", number(m.total_tokens)),
            &tokens_detail,
            inner,
        )),
        frame(&pair(
            &format!("Cache {cache:.0}%"),
            &format!("Raciocínio {}", number(m.tokens.reasoning_output_tokens)),
            inner,
        )),
        frame(&pair(&cost, &cost_note, inner)),
        frame(&pair(&active, &agents, inner)),
        frame(&if inner >= 44 {
            pair(&spark, "consumo no período", inner)
        } else {
            spark
        }),
        frame(&format!("Consumo {trend}")),
        frame(&status),
        frame(&footer),
        bottom,
    ]
}

struct TerminalGuard {
    colors: bool,
}
impl TerminalGuard {
    fn enter(colors: bool) -> Result<Self> {
        terminal::enable_raw_mode()?;
        let guard = Self { colors };
        execute!(io::stdout(), cursor::Hide, terminal::DisableLineWrap)?;
        Ok(guard)
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        if self.colors {
            let _ = execute!(stdout, ResetColor, SetAttribute(Attribute::Reset));
        }
        let _ = execute!(stdout, cursor::Show, terminal::EnableLineWrap);
        let _ = terminal::disable_raw_mode();
    }
}

pub struct Options<'a> {
    pub sessions: &'a Path,
    pub period: Period,
    pub timezone: Tz,
    pub scope: Scope,
    pub once: bool,
    pub line: bool,
    pub no_sync: bool,
    pub interval: u64,
    pub quality: bool,
}

pub fn render_feedback(
    jobs: &[Execution],
    timezone: Tz,
    scope: &Scope,
    width: usize,
) -> Result<Vec<String>> {
    let jobs: Vec<_> = jobs
        .iter()
        .filter(|j| {
            scope.project.as_ref().is_none_or(|p| &j.project == p)
                && scope
                    .run
                    .as_ref()
                    .is_none_or(|r| j.root_id.as_ref() == Some(r))
        })
        .cloned()
        .collect();
    let trends = trend::daily(&jobs, Utc::now(), timezone, 24, 0.3, None, None)?;
    let selected = trends.iter().max_by_key(|t| t.last_execution);
    let inner = width.clamp(30, 68) - 4;
    let frame = |text: &str| frame(text, inner);
    let mut content = if let Some(t) = selected {
        vec![
            pair(
                "ENTREGA",
                &format!(
                    "{} · rev {}",
                    t.profile,
                    &t.revision[..t.revision.len().min(8)]
                ),
                inner,
            ),
            "24 dias · 0 a 100% · grupo mais recente".into(),
            format!(
                "Entrega  {}",
                trend::spark(t.points.iter().map(|p| p.delivery))
            ),
            format!(
                "Softline {}",
                trend::spark(t.points.iter().map(|p| p.smooth_delivery))
            ),
            format!(
                "Rapidez  {}",
                trend::spark(t.points.iter().map(|p| p.smooth_speed))
            ),
            format!(
                "{} avaliações · EWMA α=0.3 · · sem avaliação",
                t.feedback_count
            ),
            t.signal.clone(),
            "Mesmo perfil, projeto e stack observada".into(),
        ]
    } else {
        vec![
            "ENTREGA · feedback diário".into(),
            "Ainda não há execuções pelo StackPulse.".into(),
            "Use run \"pedido\" e avalie a entrega ao final.".into(),
            "".into(),
            "Softline = média móvel exponencial diária".into(),
            "Rapidez é sua percepção, registrada de 1 a 5.".into(),
            "Feedback ausente não conta como nota zero.".into(),
            "".into(),
        ]
    };
    content.push(if inner >= 44 {
        pair("g:consumo  q:sair", "detalhes: trend", inner)
    } else {
        "g:consumo  q:sair".into()
    });
    let mut lines = vec![format!("╭{}╮", "─".repeat(inner + 2))];
    lines.extend(content.iter().map(|s| frame(s)));
    lines.push(format!("╰{}╯", "─".repeat(inner + 2)));
    Ok(lines)
}

fn supports_color(tty: bool, no_color: bool, term: Option<&str>) -> bool {
    tty && !no_color && term != Some("dumb")
}

// Keep renderers free of ANSI controls: presentation is applied only when writing to a TTY.
fn write_line(
    out: &mut impl Write,
    line: &str,
    row: usize,
    quality: bool,
    colors: bool,
) -> Result<()> {
    if !colors {
        write!(out, "{line}")?;
        return Ok(());
    }
    queue!(out, SetForegroundColor(Color::DarkCyan))?;
    if let Some(content) = line.strip_prefix("│ ").and_then(|s| s.strip_suffix(" │")) {
        write!(out, "│ ")?;
        let chart = if quality {
            (3..=5).contains(&row)
        } else {
            row == 6
        };
        let color = if row == 1 || chart {
            Color::Cyan
        } else if (!quality && [2, 4, 5].contains(&row)) || (quality && row == 7) {
            Color::Reset
        } else {
            Color::DarkGrey
        };
        queue!(out, SetForegroundColor(color))?;
        if row == 1 || (!quality && row == 2) {
            queue!(out, SetAttribute(Attribute::Bold))?;
        }
        write!(out, "{content}")?;
        queue!(
            out,
            SetAttribute(Attribute::Reset),
            SetForegroundColor(Color::DarkCyan)
        )?;
        write!(out, " │")?;
    } else {
        write!(out, "{line}")?;
    }
    queue!(out, ResetColor, SetAttribute(Attribute::Reset))?;
    Ok(())
}

pub fn run(db: &mut Db, options: Options<'_>) -> Result<()> {
    let colors = supports_color(
        io::stdout().is_terminal(),
        std::env::var_os("NO_COLOR").is_some(),
        std::env::var("TERM").ok().as_deref(),
    );
    let live =
        !options.once && !options.line && io::stdout().is_terminal() && io::stdin().is_terminal();
    let mut period = options.period;
    let mut quality = options.quality;
    let mut sync = None;
    let mut data;
    let label = options
        .scope
        .run
        .as_deref()
        .or(options
            .scope
            .project
            .as_deref()
            .and_then(|p| Path::new(p).file_name().and_then(|n| n.to_str())))
        .unwrap_or("todos");
    if live {
        ensure!(
            terminal::size()?.1 >= 13,
            "O widget precisa de pelo menos 13 linhas no terminal; use --line"
        );
    }
    if !options.no_sync {
        sync = Some(collector::sync(db, options.sessions)?);
    }
    data = db.snapshot()?;
    let _guard = if live {
        Some(TerminalGuard::enter(colors)?)
    } else {
        None
    };
    let mut next_sync = Instant::now() + Duration::from_secs(options.interval.max(1));
    let mut drawn = 0_u16;
    let mut dirty = true;
    loop {
        if dirty {
            if live {
                let (columns, rows) = terminal::size()?;
                ensure!(
                    columns >= 32 && rows >= 13,
                    "Terminal pequeno: use widget --line (mínimo interativo: 32×13)"
                );
            }
            let report =
                analytics::report(&data, &options.scope, period, Utc::now(), options.timezone)?;
            if options.line {
                println!("{}", one_line(&report));
                return Ok(());
            }
            let width = terminal::size().map_or(68, |(w, _)| usize::from(w).saturating_sub(1));
            let lines = if quality {
                render_feedback(&db.executions()?, options.timezone, &options.scope, width)?
            } else {
                render(&report, sync.as_ref(), width, label, live)
            };
            let mut stdout = io::stdout().lock();
            if live {
                if drawn > 0 {
                    queue!(stdout, cursor::MoveUp(drawn), cursor::MoveToColumn(0))?;
                }
                for (row, line) in lines.iter().enumerate() {
                    queue!(stdout, terminal::Clear(ClearType::CurrentLine))?;
                    write_line(&mut stdout, line, row, quality, colors)?;
                    write!(stdout, "\r\n")?;
                }
                stdout.flush()?;
                drawn = lines.len() as u16;
            } else {
                for (row, line) in lines.iter().enumerate() {
                    write_line(&mut stdout, line, row, quality, colors)?;
                    writeln!(stdout)?;
                }
                return Ok(());
            }
            dirty = false;
        }
        if event::poll(Duration::from_millis(150))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Char('h') => {
                        period = Period::Hour;
                        dirty = true;
                    }
                    KeyCode::Char('d') => {
                        period = Period::Day;
                        dirty = true;
                    }
                    KeyCode::Char('m') => {
                        period = Period::Month;
                        dirty = true;
                    }
                    KeyCode::Char('g') => {
                        quality = !quality;
                        dirty = true;
                    }
                    _ => {}
                },
                Event::Resize(_, _) => dirty = true,
                _ => {}
            }
        }
        if Instant::now() >= next_sync {
            if !options.no_sync {
                match collector::sync(db, options.sessions) {
                    Ok(value) => sync = Some(value),
                    Err(error) => {
                        sync = Some(ImportReport {
                            errors: vec![error.to_string()],
                            ..Default::default()
                        })
                    }
                }
            }
            data = db.snapshot()?;
            next_sync = Instant::now() + Duration::from_secs(options.interval.max(1));
            dirty = true;
        }
    }
    Ok(())
}
