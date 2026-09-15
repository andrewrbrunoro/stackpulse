use crate::{widget, workflow::Execution};
use anyhow::{Result, ensure};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::Tz;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Serialize)]
pub struct Point {
    pub day: NaiveDate,
    pub executions: usize,
    pub rated: usize,
    pub delivery: Option<f64>,
    pub speed: Option<f64>,
    pub smooth_delivery: Option<f64>,
    pub smooth_speed: Option<f64>,
    pub tokens: Option<u64>,
    pub wall_ms: u64,
    pub estimated_usd: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct Trend {
    pub cohort_id: String,
    pub last_execution: DateTime<Utc>,
    pub profile: String,
    pub revision: String,
    pub project: String,
    pub benchmark: String,
    pub observed_stack: Option<String>,
    pub coverage: String,
    pub max_agents: u32,
    pub alpha: f64,
    pub points: Vec<Point>,
    pub feedback_count: usize,
    pub signal: String,
}

pub fn daily(
    jobs: &[Execution],
    now: DateTime<Utc>,
    tz: Tz,
    days: u32,
    alpha: f64,
    profile: Option<&str>,
    benchmark: Option<&str>,
) -> Result<Vec<Trend>> {
    ensure!((2..=366).contains(&days), "days deve estar entre 2 e 366");
    ensure!(
        alpha.is_finite() && alpha > 0.0 && alpha <= 1.0,
        "alpha deve estar entre 0 (exclusivo) e 1"
    );
    let today = now.with_timezone(&tz).date_naive();
    let from = today - Duration::days(i64::from(days) - 1);
    let mut groups = BTreeMap::<String, Vec<&Execution>>::new();
    for j in jobs.iter().filter(|j| {
        j.kind == "task"
            && j.ended_at.is_some()
            && profile.is_none_or(|p| p == j.profile_name)
            && benchmark.is_none_or(|b| b == j.benchmark)
    }) {
        let day = j.ended_at.unwrap().with_timezone(&tz).date_naive();
        if day < from || day > today {
            continue;
        }
        let key = serde_json::to_string(&(
            &j.profile_sha256,
            &j.project,
            &j.benchmark,
            &j.observed_stack,
            &j.coverage,
            j.max_agents,
            &j.sandbox,
            j.execution_client,
            if j.benchmark.is_empty() {
                None
            } else {
                Some(&j.prompt_sha256)
            },
        ))?;
        groups.entry(key).or_default().push(j);
    }
    let mut trends = Vec::new();
    for (key, group) in &mut groups {
        group.sort_by_key(|j| j.ended_at);
        let first = group[0];
        let mut points = Vec::new();
        let mut smooth_q = None;
        let mut smooth_s = None;
        let mut ratings = Vec::new();
        for offset in 0..days {
            let day = from + Duration::days(i64::from(offset));
            let same: Vec<_> = group
                .iter()
                .filter(|j| j.ended_at.unwrap().with_timezone(&tz).date_naive() == day)
                .collect();
            let feedbacks: Vec<_> = same.iter().filter_map(|j| j.feedback.as_ref()).collect();
            let count = feedbacks.len();
            let mean = |values: Vec<f64>| {
                if values.is_empty() {
                    None
                } else {
                    Some(values.iter().sum::<f64>() / values.len() as f64)
                }
            };
            let delivery = mean(feedbacks.iter().map(|f| f.delivered).collect());
            let speed = mean(
                feedbacks
                    .iter()
                    .filter(|f| (1..=5).contains(&f.speed))
                    .map(|f| (f.speed as f64 - 1.0) / 4.0)
                    .collect(),
            );
            ratings.extend(feedbacks.iter().map(|f| f.delivered));
            // Missing days are gaps, not zeros and not invented ratings.
            let mut smooth = |q: Option<f64>, s: Option<f64>| {
                let a = q.map(|q| {
                    let v = smooth_q.map_or(q, |prev: f64| alpha * q + (1.0 - alpha) * prev);
                    smooth_q = Some(v);
                    v
                });
                let b = s.map(|s| {
                    let v = smooth_s.map_or(s, |prev: f64| alpha * s + (1.0 - alpha) * prev);
                    smooth_s = Some(v);
                    v
                });
                (a, b)
            };
            let (sd, ss) = smooth(delivery, speed);
            let token_values: Option<Vec<_>> = same
                .iter()
                .map(|j| {
                    j.metrics
                        .as_ref()
                        .map(|m| m.total_tokens)
                        .or(j.reported_tokens.map(|t| t.total()))
                })
                .collect();
            let cost_values: Option<Vec<_>> = same
                .iter()
                .map(|j| {
                    j.metrics
                        .as_ref()
                        .and_then(|m| m.estimated_usd)
                        .or(j.reported_cost_usd)
                })
                .collect();
            points.push(Point {
                day,
                executions: same.len(),
                rated: count,
                delivery,
                speed,
                smooth_delivery: sd,
                smooth_speed: ss,
                tokens: if same.is_empty() {
                    None
                } else {
                    token_values.map(|v| v.iter().sum())
                },
                wall_ms: same.iter().filter_map(|j| j.wall_ms).sum(),
                estimated_usd: if same.is_empty() {
                    None
                } else {
                    cost_values.map(|v| v.iter().sum())
                },
            });
        }
        let rated_days = points.iter().filter(|p| p.rated > 0).count();
        let signal = if first.benchmark.is_empty() {
            "Feedback cotidiano; não atribui mudanças ao provedor"
        } else if ratings.len() < 10 || rated_days < 3 {
            "Ainda sem repetição suficiente para avaliar a tendência"
        } else {
            let recent = &ratings[ratings.len() - 5..];
            let base = &ratings[..ratings.len() - 5];
            let delta =
                recent.iter().sum::<f64>() / 5.0 - base.iter().sum::<f64>() / base.len() as f64;
            if delta <= -0.1 {
                "Queda de entrega percebida ≥10 pp; investigar com compare"
            } else {
                "Sem queda ≥10 pp neste recorte; não comprova equivalência"
            }
        };
        trends.push(Trend {
            cohort_id: crate::profiles::digest(key.as_bytes())[..8].to_string(),
            last_execution: group.iter().filter_map(|j| j.ended_at).max().unwrap(),
            profile: first.profile_name.clone(),
            revision: first.profile_sha256.clone(),
            project: first.project.clone(),
            benchmark: first.benchmark.clone(),
            observed_stack: first.observed_stack.clone(),
            coverage: first.coverage.clone(),
            max_agents: first.max_agents,
            alpha,
            feedback_count: ratings.len(),
            points,
            signal: signal.into(),
        });
    }
    Ok(trends)
}

pub fn spark(points: impl Iterator<Item = Option<f64>>) -> String {
    let levels = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    points
        .map(|v| v.map_or('·', |v| levels[(v.clamp(0.0, 1.0) * 7.0).round() as usize]))
        .collect()
}

pub fn render(t: &Trend) -> String {
    format!(
        "{} · rev {} · grupo {} · {} feedbacks\nEntrega   {}\nSoftline  {}\nRapidez   {}\n          {} → {} · EWMA α={}\n{}\n",
        widget::clean(&t.profile, 50),
        &t.revision[..t.revision.len().min(8)],
        t.cohort_id,
        t.feedback_count,
        spark(t.points.iter().map(|p| p.delivery)),
        spark(t.points.iter().map(|p| p.smooth_delivery)),
        spark(t.points.iter().map(|p| p.smooth_speed)),
        t.points.first().unwrap().day,
        t.points.last().unwrap().day,
        t.alpha,
        t.signal
    )
}
