use crate::model::*;
use anyhow::{Result, anyhow, ensure};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use clap::ValueEnum;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone, Copy, Default, ValueEnum, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Period {
    Hour,
    #[default]
    Day,
    Month,
}

impl Period {
    pub fn label(self) -> &'static str {
        match self {
            Self::Hour => "HORA",
            Self::Day => "DIA",
            Self::Month => "MÊS",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Scope {
    pub project: Option<String>,
    pub run: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, serde::Deserialize)]
pub struct Metrics {
    pub tokens: Tokens,
    pub total_tokens: u64,
    pub estimated_usd: Option<f64>,
    pub priced_events: usize,
    pub events: usize,
    pub agents: usize,
    pub runs: usize,
    pub active_ms: i64,
    pub agent_ms: i64,
    pub open_turns: usize,
    pub untimed_events: usize,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub period: Period,
    pub timezone: String,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub current: Metrics,
    pub previous: Metrics,
    pub token_change_pct: Option<f64>,
    pub series: Vec<u64>,
    pub configurations: Vec<(String, u64)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunReport {
    pub id: String,
    pub label: String,
    pub project: String,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub configuration: String,
    pub missing_parent: bool,
    pub metrics: Metrics,
    pub annotation: Option<Annotation>,
}

pub fn root_id(id: &str, sessions: &HashMap<&str, &Session>) -> String {
    let mut current = id;
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(current) {
            return id.into();
        }
        match sessions.get(current).and_then(|s| s.parent_id.as_deref()) {
            Some(parent) => current = parent,
            None => return current.into(),
        }
    }
}

fn selected(data: &Dataset, scope: &Scope) -> HashSet<String> {
    let sessions: HashMap<_, _> = data.sessions.iter().map(|s| (s.id.as_str(), s)).collect();
    let roots: HashSet<_> = data
        .sessions
        .iter()
        .filter(|s| {
            scope.project.as_ref().is_none_or(|p| s.project == *p)
                && scope
                    .run
                    .as_ref()
                    .is_none_or(|r| root_id(&s.id, &sessions) == *r)
        })
        .map(|s| root_id(&s.id, &sessions))
        .collect();
    data.sessions
        .iter()
        .filter(|s| roots.contains(&root_id(&s.id, &sessions)))
        .map(|s| s.id.clone())
        .collect()
}

fn cost(data: &Dataset, usage: &Usage, provider: &str) -> Option<f64> {
    data.prices
        .iter()
        .filter(|p| {
            p.provider == provider
                && p.model == usage.model
                && p.service_tier == usage.service_tier
                && p.effective_at <= usage.at
        })
        .max_by_key(|p| p.effective_at)
        .map(|p| p.estimate(usage.tokens))
}

pub fn union_ms(intervals: &[(DateTime<Utc>, DateTime<Utc>)]) -> i64 {
    let mut intervals = intervals.to_vec();
    intervals.sort_unstable();
    let mut merged: Option<(DateTime<Utc>, DateTime<Utc>)> = None;
    let mut total = 0;
    for (start, end) in intervals.into_iter().filter(|(a, b)| b > a) {
        match merged {
            Some((a, b)) if start <= b => merged = Some((a, b.max(end))),
            Some((a, b)) => {
                total += (b - a).num_milliseconds();
                merged = Some((start, end));
            }
            None => merged = Some((start, end)),
        }
    }
    total + merged.map_or(0, |(a, b)| (b - a).num_milliseconds())
}

pub fn metrics(
    data: &Dataset,
    ids: &HashSet<String>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Metrics {
    let sessions: HashMap<_, _> = data.sessions.iter().map(|s| (s.id.as_str(), s)).collect();
    let mut result = Metrics::default();
    let mut agents = HashSet::new();
    let mut roots = HashSet::new();
    let mut estimate = 0.0;
    let mut last_observed = HashMap::<(&str, &str), DateTime<Utc>>::new();
    let turn_keys: HashSet<_> = data
        .turns
        .iter()
        .map(|t| (t.session_id.as_str(), t.id.as_str()))
        .collect();
    for usage in &data.usage {
        if let Some(turn_id) = usage.turn_id.as_deref() {
            last_observed
                .entry((&usage.session_id, turn_id))
                .and_modify(|v| *v = (*v).max(usage.at))
                .or_insert(usage.at);
        }
        if !ids.contains(&usage.session_id) || usage.at < from || usage.at >= to {
            continue;
        }
        result.tokens.add(usage.tokens);
        result.events += 1;
        if usage
            .turn_id
            .as_ref()
            .is_none_or(|t| !turn_keys.contains(&(usage.session_id.as_str(), t.as_str())))
        {
            result.untimed_events += 1;
        }
        agents.insert(usage.session_id.clone());
        roots.insert(root_id(&usage.session_id, &sessions));
        if let Some(session) = sessions.get(usage.session_id.as_str())
            && let Some(value) = cost(data, usage, &session.provider)
        {
            result.priced_events += 1;
            estimate += value;
        }
    }
    let mut intervals = Vec::new();
    for turn in &data.turns {
        if !ids.contains(&turn.session_id) {
            continue;
        }
        // Open turns stop at their latest telemetry; a stale session must not accrue time forever.
        let end = turn
            .ended_at
            .or_else(|| {
                last_observed
                    .get(&(turn.session_id.as_str(), turn.id.as_str()))
                    .copied()
            })
            .unwrap_or(turn.started_at)
            .min(to);
        let start = turn.started_at.max(from);
        if turn.ended_at.is_none()
            && ((turn.started_at >= from && turn.started_at < to) || end > start)
        {
            result.open_turns += 1;
        }
        if end <= start {
            continue;
        }
        agents.insert(turn.session_id.clone());
        roots.insert(root_id(&turn.session_id, &sessions));
        result.agent_ms += (end - start).num_milliseconds();
        intervals.push((start, end));
    }
    result.active_ms = union_ms(&intervals);
    result.agents = agents.len();
    result.runs = roots.len();
    result.total_tokens = result.tokens.total();
    if result.events > 0 && result.priced_events == result.events {
        result.estimated_usd = Some(estimate);
    }
    result
}

fn midnight(date: NaiveDate, tz: Tz) -> Result<DateTime<Utc>> {
    let naive = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| anyhow!("Data inválida"))?;
    // Some zones skip midnight during daylight-saving transitions.
    (0..180)
        .find_map(|m| {
            tz.from_local_datetime(&(naive + Duration::minutes(m)))
                .earliest()
        })
        .map(|d| d.with_timezone(&Utc))
        .ok_or_else(|| anyhow!("Data inexistente no fuso"))
}

pub fn window(
    period: Period,
    now: DateTime<Utc>,
    tz: Tz,
) -> Result<(DateTime<Utc>, DateTime<Utc>, DateTime<Utc>)> {
    let local = now.with_timezone(&tz);
    let (start, prev) = match period {
        Period::Hour => {
            let start = now
                - Duration::seconds(i64::from(local.minute() * 60 + local.second()))
                - Duration::nanoseconds(i64::from(local.nanosecond()));
            (start, start - Duration::hours(1))
        }
        Period::Day => {
            let date = local.date_naive();
            (
                midnight(date, tz)?,
                midnight(
                    date.pred_opt()
                        .ok_or_else(|| anyhow!("Data fora do intervalo"))?,
                    tz,
                )?,
            )
        }
        Period::Month => {
            let date = local
                .date_naive()
                .with_day(1)
                .ok_or_else(|| anyhow!("Mês inválido"))?;
            let prior = date
                .pred_opt()
                .and_then(|d| d.with_day(1))
                .ok_or_else(|| anyhow!("Data fora do intervalo"))?;
            (midnight(date, tz)?, midnight(prior, tz)?)
        }
    };
    // Compare equal elapsed exposure, never a partial current day against a complete prior day.
    Ok((start, prev, (prev + (now - start)).min(start)))
}

pub fn report(
    data: &Dataset,
    scope: &Scope,
    period: Period,
    now: DateTime<Utc>,
    tz: Tz,
) -> Result<Report> {
    let (from, previous_start, previous_end) = window(period, now, tz)?;
    let ids = selected(data, scope);
    let current = metrics(data, &ids, from, now);
    let previous = metrics(data, &ids, previous_start, previous_end);
    let token_change_pct = if now - from == previous_end - previous_start {
        change(current.total_tokens as f64, previous.total_tokens as f64)
    } else {
        None
    };
    let mut series = vec![0; 24];
    let mut configurations = BTreeMap::<String, u64>::new();
    let span = (now - from).num_milliseconds().max(1);
    for u in data
        .usage
        .iter()
        .filter(|u| ids.contains(&u.session_id) && u.at >= from && u.at < now)
    {
        let index =
            (((u.at - from).num_milliseconds() as f64 / span as f64) * 24.0).floor() as usize;
        series[index.min(23)] += u.tokens.total();
        *configurations
            .entry(format!("{} / {}", u.model, u.effort))
            .or_default() += u.tokens.total();
    }
    let mut configurations: Vec<_> = configurations.into_iter().collect();
    configurations.sort_by(|a, b| b.1.cmp(&a.1));
    Ok(Report {
        period,
        timezone: tz.to_string(),
        from,
        to: now,
        current,
        previous,
        token_change_pct,
        series,
        configurations,
    })
}

pub fn runs(data: &Dataset, scope: &Scope) -> Vec<RunReport> {
    let sessions: HashMap<_, _> = data.sessions.iter().map(|s| (s.id.as_str(), s)).collect();
    let selected = selected(data, scope);
    let mut groups = BTreeMap::<String, HashSet<String>>::new();
    for s in &data.sessions {
        if selected.contains(&s.id) {
            groups
                .entry(root_id(&s.id, &sessions))
                .or_default()
                .insert(s.id.clone());
        }
    }
    let mut results = Vec::new();
    for (root, ids) in groups {
        let own: Vec<_> = data
            .usage
            .iter()
            .filter(|u| ids.contains(&u.session_id))
            .collect();
        let Some(last) = own.iter().map(|u| u.at).max() else {
            continue;
        };
        let first = data
            .sessions
            .iter()
            .filter(|s| ids.contains(&s.id))
            .map(|s| s.created_at)
            .min()
            .unwrap_or(last);
        let end = data
            .turns
            .iter()
            .filter(|t| ids.contains(&t.session_id))
            .filter_map(|t| t.ended_at)
            .max()
            .unwrap_or(last)
            .max(last)
            + Duration::milliseconds(1);
        let mut composition = BTreeMap::<String, usize>::new();
        for id in &ids {
            let mut configs: Vec<_> = own
                .iter()
                .filter(|u| &u.session_id == id)
                .map(|u| {
                    let provider = sessions
                        .get(id.as_str())
                        .map_or("unknown", |s| s.provider.as_str());
                    format!("{provider}:{}:{}:{}", u.model, u.effort, u.service_tier)
                })
                .collect();
            configs.sort();
            configs.dedup();
            let role = if id == &root { "root" } else { "worker" };
            *composition
                .entry(format!("{role}[{}]", configs.join("→")))
                .or_default() += 1;
        }
        let configuration = composition
            .iter()
            .map(|(k, n)| format!("{n}×{k}"))
            .collect::<Vec<_>>()
            .join(" + ");
        let annotation = data.annotations.iter().find(|a| a.run_id == root).cloned();
        let session = sessions.get(root.as_str()).copied();
        results.push(RunReport {
            id: root.clone(),
            label: annotation
                .as_ref()
                .filter(|a| !a.label.is_empty())
                .map(|a| a.label.clone())
                .unwrap_or_else(|| root.clone()),
            project: session.map_or_else(String::new, |s| s.project.clone()),
            first_seen: first,
            last_seen: last,
            configuration,
            missing_parent: session.is_none(),
            metrics: metrics(data, &ids, first, end),
            annotation,
        });
    }
    results.sort_by_key(|r| std::cmp::Reverse(r.last_seen));
    results
}

#[derive(Debug, Serialize)]
pub struct Comparison {
    pub benchmark: String,
    pub configuration: String,
    pub baseline_n: usize,
    pub current_n: usize,
    pub quality_change_pp: Option<f64>,
    pub quality_ci95_pp: Option<[f64; 2]>,
    pub tokens_per_quality_change_pct: Option<f64>,
    pub time_median_change_pct: Option<f64>,
    pub regression_signal: bool,
    pub interpretation: String,
}

fn mean(xs: &[f64]) -> f64 {
    xs.iter().sum::<f64>() / xs.len() as f64
}
fn median(mut xs: Vec<f64>) -> f64 {
    xs.sort_by(f64::total_cmp);
    let m = xs.len() / 2;
    if xs.len().is_multiple_of(2) {
        (xs[m - 1] + xs[m]) / 2.0
    } else {
        xs[m]
    }
}
fn change(current: f64, previous: f64) -> Option<f64> {
    (previous > 0.0).then_some((current / previous - 1.0) * 100.0)
}

// Deterministic percentile bootstrap of the difference of independent sample means.
fn confidence_interval(base: &[f64], current: &[f64]) -> [f64; 2] {
    let mut state = 0x9e3779b97f4a7c15_u64;
    let mut sample = |xs: &[f64]| -> f64 {
        let mut sum = 0.0;
        for _ in xs {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            sum += xs[(state as usize) % xs.len()];
        }
        sum / xs.len() as f64
    };
    let mut deltas: Vec<f64> = (0..2000).map(|_| sample(current) - sample(base)).collect();
    deltas.sort_by(f64::total_cmp);
    [deltas[50] * 100.0, deltas[1949] * 100.0]
}

pub fn comparisons(runs: &[RunReport]) -> Vec<Comparison> {
    let mut groups = BTreeMap::<(String, String), (Vec<&RunReport>, Vec<&RunReport>)>::new();
    for r in runs {
        let Some(a) = r
            .annotation
            .as_ref()
            .filter(|a| !a.benchmark.is_empty() && a.quality.is_some())
        else {
            continue;
        };
        if r.missing_parent || r.metrics.open_turns > 0 || r.metrics.untimed_events > 0 {
            continue;
        }
        let group = groups
            .entry((a.benchmark.clone(), r.configuration.clone()))
            .or_default();
        if a.baseline {
            group.0.push(r);
        } else {
            group.1.push(r);
        }
    }
    groups
        .into_iter()
        .map(|((benchmark, configuration), (base, current))| {
            let mut c = Comparison {
                benchmark,
                configuration,
                baseline_n: base.len(),
                current_n: current.len(),
                quality_change_pp: None,
                quality_ci95_pp: None,
                tokens_per_quality_change_pct: None,
                time_median_change_pct: None,
                regression_signal: false,
                interpretation: "Sem amostras nos dois grupos".into(),
            };
            if base.is_empty() || current.is_empty() {
                return c;
            }
            let qualities = |rs: &[&RunReport]| -> Vec<f64> {
                rs.iter()
                    .map(|r| r.annotation.as_ref().and_then(|a| a.quality).unwrap_or(0.0))
                    .collect()
            };
            let bq = qualities(&base);
            let cq = qualities(&current);
            let delta = (mean(&cq) - mean(&bq)) * 100.0;
            c.quality_change_pp = Some(delta);
            let bsum = bq.iter().sum::<f64>();
            let csum = cq.iter().sum::<f64>();
            if bsum > 0.0 && csum > 0.0 {
                c.tokens_per_quality_change_pct = change(
                    current
                        .iter()
                        .map(|r| r.metrics.total_tokens as f64)
                        .sum::<f64>()
                        / csum,
                    base.iter()
                        .map(|r| r.metrics.total_tokens as f64)
                        .sum::<f64>()
                        / bsum,
                );
            }
            c.time_median_change_pct = change(
                median(current.iter().map(|r| r.metrics.active_ms as f64).collect()),
                median(base.iter().map(|r| r.metrics.active_ms as f64).collect()),
            );
            c.interpretation = "Amostra pequena; reúna pelo menos 5 execuções por grupo".into();
            if base.len() >= 5 && current.len() >= 5 {
                let ci = confidence_interval(&bq, &cq);
                c.quality_ci95_pp = Some(ci);
                c.regression_signal = delta <= -5.0 && ci[1] < 0.0;
                c.interpretation = if c.regression_signal {
                    "Sinal exploratório de regressão; investigar causas"
                } else {
                    "Sem sinal de regressão pelo critério; não comprova equivalência"
                }
                .into();
            }
            c
        })
        .collect()
}

pub fn resolve_run(data: &Dataset, prefix: &str) -> Result<String> {
    let matches: Vec<_> = runs(data, &Scope::default())
        .into_iter()
        .filter(|r| r.id.starts_with(prefix))
        .collect();
    ensure!(
        matches.len() == 1,
        "ID deve identificar exatamente uma execução ({} correspondências)",
        matches.len()
    );
    Ok(matches[0].id.clone())
}
