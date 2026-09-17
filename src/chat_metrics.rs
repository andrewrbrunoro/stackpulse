//! Conversation-local measurements. Missing telemetry and feedback stay missing.
use crate::{widget, workflow::Execution};
use std::collections::HashSet;

#[derive(Clone, Debug)]
pub(crate) struct PromptStats {
    pub(crate) number: usize,
    pub(crate) title: String,
    pub(crate) tokens: Option<u64>,
    pub(crate) elapsed_ms: Option<u64>,
    pub(crate) delivery_score: Option<f64>,
    pub(crate) speed: Option<u8>,
    pub(crate) finished: bool,
    pub(crate) partial: bool,
    pub(crate) execution_id: Option<String>,
}

impl PromptStats {
    pub(crate) fn from_execution(
        number: usize,
        title: &str,
        finished: bool,
        execution: Option<&Execution>,
    ) -> Self {
        let metrics = execution
            .and_then(|job| job.metrics.as_ref())
            .filter(|metrics| metrics.events > 0);
        // Logs and the CLI may describe the same usage. Choose one source,
        // including an explicitly measured zero, instead of adding both.
        let tokens = match metrics {
            Some(metrics) => (metrics.tokens.valid()
                && metrics.total_tokens == metrics.tokens.total())
            .then_some(metrics.total_tokens),
            None => execution
                .and_then(|job| job.reported_tokens)
                .filter(|tokens| tokens.valid())
                .map(|tokens| tokens.total()),
        };
        let feedback = execution.and_then(|job| job.feedback.as_ref());
        let delivery_score = feedback
            .map(|feedback| feedback.delivered)
            .filter(|score| score.is_finite() && (0.0..=1.0).contains(score))
            .map(|score| score * 10.0);
        let speed = feedback
            .map(|feedback| feedback.speed)
            .filter(|speed| (1..=5).contains(speed));
        let can_rate = execution.is_some_and(|job| job.ended_at.is_some());
        let partial = !finished
            || !can_rate
            || tokens.is_none()
            || execution
                .is_none_or(|job| !matches!(job.coverage.as_str(), "local_observed" | "cli_tree"))
            || metrics.is_some_and(|metrics| metrics.open_turns > 0 || metrics.untimed_events > 0);
        Self {
            number,
            title: widget::clean(
                &title.split_whitespace().collect::<Vec<_>>().join(" "),
                usize::MAX,
            ),
            tokens,
            elapsed_ms: execution.and_then(|job| job.wall_ms),
            delivery_score,
            speed,
            finished,
            partial,
            execution_id: execution.map(|job| job.id.clone()),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ConversationStats {
    pub(crate) prompts: Vec<PromptStats>,
    pub(crate) total_tokens: Option<u64>,
    pub(crate) measured: usize,
    pub(crate) rated: usize,
    pub(crate) score: Option<f64>,
    pub(crate) partial: bool,
}

impl ConversationStats {
    pub(crate) fn from_prompts(prompts: Vec<PromptStats>) -> Self {
        // A restored snapshot can mention one execution more than once. Keep
        // every row for navigation, but use its latest snapshot once in totals.
        let mut seen = HashSet::new();
        let sources: Vec<_> = prompts
            .iter()
            .rev()
            .filter(|prompt| {
                prompt
                    .execution_id
                    .as_deref()
                    .is_none_or(|id| seen.insert(id))
            })
            .collect();
        let measured = sources
            .iter()
            .filter(|prompt| prompt.tokens.is_some())
            .count();
        let total_tokens = if !prompts.is_empty() && measured == 0 {
            None
        } else {
            sources
                .iter()
                .filter_map(|prompt| prompt.tokens)
                .try_fold(0_u64, u64::checked_add)
        };
        let (rated, score_sum) = sources
            .iter()
            .filter_map(|prompt| prompt.delivery_score)
            .filter(|score| score.is_finite() && (0.0..=10.0).contains(score))
            .fold((0_usize, 0.0), |(count, sum), score| {
                (count + 1, sum + score)
            });
        let score = (rated > 0).then(|| score_sum / rated as f64);
        let partial = sources.iter().any(|prompt| prompt.partial)
            || measured < sources.len()
            || total_tokens.is_none();
        Self {
            prompts,
            total_tokens,
            measured,
            rated,
            score,
            partial,
        }
    }

    /// Full detail for a scrollable transcript; the compact widgets may show a
    /// smaller window, but neither the total nor this list drops older prompts.
    pub(crate) fn detail_lines(&self) -> Vec<String> {
        let mut lines = vec![
            "MÉTRICAS DESTA CONVERSA".into(),
            format!(
                "Total observado: {} · nota média: {}",
                self.total_tokens
                    .map(|tokens| format!("{tokens} tokens"))
                    .unwrap_or_else(|| "tokens indisponíveis".into()),
                score_label(self.score),
            ),
            format!(
                "{} pedidos · {} medições de tokens · {} avaliações de entrega",
                self.prompts.len(),
                self.measured,
                self.rated,
            ),
            "Cada execução entra uma vez no total e na média, mesmo que apareça em mais de um pedido.".into(),
            "Nota: média aritmética das avaliações de entrega, em escala 0..10 (entrega × 10).".into(),
            "Somente pedidos avaliados entram na média; a resposta da LLM não define a nota.".into(),
            "Excelente = 10 · Bom = 7,5 · Mediano = 5 · Ruim = 0. Você pode alterar sua avaliação.".into(),
            "Rapidez: avaliação separada de 1..5; não altera a nota de entrega.".into(),
        ];
        if self.partial {
            lines.push("Consumo parcial: faltam medições, há pedidos em andamento ou cobertura incompleta.".into());
        }
        if self.prompts.is_empty() {
            lines.push("Nenhum pedido nesta conversa.".into());
        }
        for prompt in &self.prompts {
            lines.push(String::new());
            lines.push(format!("#{} · {}", prompt.number, prompt.title));
            lines.push(format!(
                "{} · {} · entrega {} · rapidez {}",
                prompt
                    .tokens
                    .map(|tokens| format!("{tokens} tokens"))
                    .unwrap_or_else(|| "tokens indisponíveis".into()),
                prompt
                    .elapsed_ms
                    .map(duration)
                    .unwrap_or_else(|| "tempo indisponível".into()),
                score_label(prompt.delivery_score),
                prompt
                    .speed
                    .map(|speed| format!("{speed}/5"))
                    .unwrap_or_else(|| "sem avaliação".into()),
            ));
            lines.push(format!(
                "{} · {}",
                if prompt.finished {
                    "Finalizado"
                } else {
                    "Em andamento"
                },
                if prompt.partial {
                    "medição parcial"
                } else {
                    "medição completa"
                },
            ));
        }
        lines
    }
}

fn score_label(score: Option<f64>) -> String {
    score
        .map(|score| format!("{score:.1}/10"))
        .unwrap_or_else(|| "sem avaliação".into())
}

fn duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        let seconds = ms / 1000;
        if seconds >= 3600 {
            format!("{}h{:02}m", seconds / 3600, (seconds % 3600) / 60)
        } else if seconds >= 60 {
            format!("{}m{:02}s", seconds / 60, seconds % 60)
        } else {
            format!("{seconds}s")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        analytics::Metrics,
        client::Backend,
        model::Tokens,
        profiles::{AgentSpec, TeamSpec},
        workflow::Feedback,
    };
    use chrono::Utc;

    fn execution() -> Execution {
        Execution {
            id: "execution-fixture".into(),
            kind: "run".into(),
            project: "project".into(),
            profile_name: "team".into(),
            profile_sha256: String::new(),
            profile_markdown: String::new(),
            planned_stack: TeamSpec {
                name: "team".into(),
                provider: "openai".into(),
                orchestrator: AgentSpec {
                    provider: None,
                    role: "orchestrator".into(),
                    model: "fixture".into(),
                    effort: "medium".into(),
                    purpose: "Fixture".into(),
                    when: String::new(),
                },
                agents: vec![],
                delegation: "on_demand".into(),
                integration: String::new(),
                notes: String::new(),
            },
            observed_stack: None,
            assistant_provider: "openai".into(),
            assistant_model: "fixture".into(),
            assistant_effort: "medium".into(),
            execution_client: Backend::Codex,
            observed_model: None,
            reported_cost_usd: None,
            max_agents: 1,
            sandbox: "read-only".into(),
            prompt_sha256: String::new(),
            prompt_chars: 1,
            benchmark: String::new(),
            started_at: Utc::now(),
            ended_at: Some(Utc::now()),
            wall_ms: Some(1200),
            status: "completed".into(),
            root_id: None,
            cli_session_id: None,
            reported_tokens: None,
            metrics: None,
            coverage: "cli_tree".into(),
            error: None,
            feedback: None,
        }
    }

    fn feedback(delivered: f64, speed: u8) -> Feedback {
        Feedback {
            delivered,
            speed,
            note: String::new(),
            recorded_at: Utc::now(),
        }
    }

    fn prompt(job: &Execution) -> PromptStats {
        PromptStats::from_execution(1, "Pedido", true, Some(job))
    }

    #[test]
    fn empty_unknown_and_explicit_zero_have_distinct_totals() {
        let empty = ConversationStats::from_prompts(vec![]);
        assert_eq!(empty.total_tokens, Some(0));
        assert!(!empty.partial);
        assert_eq!(empty.score, None);

        let unknown = ConversationStats::from_prompts(vec![PromptStats::from_execution(
            1,
            "Sem registro",
            true,
            None,
        )]);
        assert_eq!(unknown.total_tokens, None);
        assert_eq!(unknown.measured, 0);
        assert!(unknown.partial);
        assert_eq!(unknown.prompts[0].execution_id, None);

        let mut job = execution();
        job.reported_tokens = Some(Tokens::default());
        let zero = ConversationStats::from_prompts(vec![prompt(&job)]);
        assert_eq!(zero.total_tokens, Some(0));
        assert_eq!(zero.measured, 1);
        assert!(!zero.partial);
        assert_eq!(
            zero.prompts[0].execution_id.as_deref(),
            Some("execution-fixture")
        );
    }

    #[test]
    fn observed_tree_wins_without_counting_root_or_cache_twice() {
        let mut job = execution();
        job.coverage = "local_observed".into();
        job.reported_tokens = Some(Tokens {
            input_tokens: 40,
            output_tokens: 10,
            ..Tokens::default()
        });
        job.metrics = Some(Metrics {
            tokens: Tokens {
                input_tokens: 100,
                cached_input_tokens: 40,
                cache_write_input_tokens: 20,
                output_tokens: 30,
                reasoning_output_tokens: 10,
            },
            total_tokens: 130,
            events: 2,
            ..Metrics::default()
        });
        assert_eq!(prompt(&job).tokens, Some(130));

        job.metrics = Some(Metrics {
            events: 1,
            ..Metrics::default()
        });
        assert_eq!(
            prompt(&job).tokens,
            Some(0),
            "An explicit zero is authoritative."
        );
        job.metrics = Some(Metrics::default());
        assert_eq!(
            prompt(&job).tokens,
            Some(50),
            "No events means there is no log measurement."
        );
    }

    #[test]
    fn invalid_counters_stay_unknown_instead_of_overflowing_or_using_a_second_source() {
        let mut job = execution();
        job.reported_tokens = Some(Tokens {
            input_tokens: u64::MAX,
            output_tokens: 1,
            ..Tokens::default()
        });
        assert_eq!(prompt(&job).tokens, None);
        job.reported_tokens = Some(Tokens {
            input_tokens: 10,
            ..Tokens::default()
        });
        job.metrics = Some(Metrics {
            tokens: Tokens {
                input_tokens: 10,
                cached_input_tokens: 11,
                ..Tokens::default()
            },
            total_tokens: 10,
            events: 1,
            ..Metrics::default()
        });
        assert_eq!(prompt(&job).tokens, None);
        job.metrics.as_mut().unwrap().tokens.cached_input_tokens = 0;
        job.metrics.as_mut().unwrap().total_tokens = 999;
        assert_eq!(prompt(&job).tokens, None);
        assert!(prompt(&job).partial);
    }

    #[test]
    fn totals_include_measured_prompts_and_preserve_missing_coverage() {
        let mut job = execution();
        job.reported_tokens = Some(Tokens {
            input_tokens: 100,
            ..Tokens::default()
        });
        let measured = prompt(&job);
        let unknown = PromptStats::from_execution(2, "Sem medição", true, None);
        let stats = ConversationStats::from_prompts(vec![measured, unknown]);
        assert_eq!(stats.total_tokens, Some(100));
        assert_eq!(stats.measured, 1);
        assert!(stats.partial);
        assert_eq!(stats.rated, 0);
        assert_eq!(stats.score, None);

        for coverage in [
            "root_only",
            "local_partial",
            "cli_partial",
            "unavailable",
            "future_coverage",
        ] {
            job.coverage = coverage.into();
            assert!(prompt(&job).partial, "{coverage}");
        }
        job.coverage = "cli_tree".into();
        job.ended_at = None;
        assert!(prompt(&job).partial);
    }

    #[test]
    fn only_valid_delivery_ratings_enter_the_arithmetic_mean() {
        let mut job = execution();
        job.reported_tokens = Some(Tokens::default());
        job.feedback = Some(feedback(0.8, 1));
        let first = prompt(&job);
        job.id = "second-execution".into();
        job.feedback = Some(feedback(0.0, 5));
        let second = prompt(&job);
        job.id = "unrated-execution".into();
        job.feedback = None;
        let unrated = prompt(&job);
        let stats = ConversationStats::from_prompts(vec![first, second, unrated]);
        assert_eq!(stats.score, Some(4.0));
        assert_eq!(stats.rated, 2);
        assert!(
            !stats.partial,
            "Rating coverage is separate from token coverage."
        );
        assert_eq!(stats.prompts[0].speed, Some(1));
        assert_eq!(stats.prompts[1].speed, Some(5));
        for delivered in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
            job.feedback = Some(feedback(delivered, 0));
            let invalid = prompt(&job);
            assert_eq!(invalid.delivery_score, None);
            assert_eq!(invalid.speed, None);
        }
        job.feedback = Some(feedback(1.0, 6));
        assert_eq!(prompt(&job).delivery_score, Some(10.0));
        assert_eq!(prompt(&job).speed, None);
    }

    #[test]
    fn quick_ratings_average_only_rated_responses_and_allow_replacement() {
        let mut job = execution();
        let mut prompts = Vec::new();
        for (index, delivery) in [Some(1.0), Some(0.5), Some(0.0), None]
            .into_iter()
            .enumerate()
        {
            job.id = format!("quick-{index}");
            job.feedback = delivery.map(|value| feedback(value, 0));
            prompts.push(prompt(&job));
        }
        let stats = ConversationStats::from_prompts(prompts.clone());
        assert_eq!(stats.score, Some(5.0));
        assert_eq!(stats.rated, 3);
        assert!(stats.prompts.iter().all(|p| p.speed.is_none()));
        prompts[2].delivery_score = Some(10.0);
        let updated = ConversationStats::from_prompts(prompts);
        assert_eq!(updated.rated, 3);
        assert!((updated.score.unwrap() - 25.0 / 3.0).abs() < 1e-10);
    }

    #[test]
    fn details_keep_every_prompt_and_exact_tokens() {
        let mut job = execution();
        job.reported_tokens = Some(Tokens {
            input_tokens: 12_345,
            ..Tokens::default()
        });
        job.feedback = Some(feedback(0.7, 3));
        let stats = ConversationStats::from_prompts(
            (1..=25)
                .map(|number| {
                    job.id = format!("execution-{number}");
                    PromptStats::from_execution(
                        number,
                        &format!("Pedido {number}"),
                        true,
                        Some(&job),
                    )
                })
                .collect(),
        );
        let detail = stats.detail_lines().join("\n");
        assert_eq!(stats.total_tokens, Some(308_625));
        assert_eq!(detail.matches("12345 tokens").count(), 25);
        assert!(detail.contains("#25 · Pedido 25"));
        assert!(detail.contains("308625 tokens"));
        assert!(detail.contains("entrega 7.0/10 · rapidez 3/5"));
        assert!(detail.contains("média aritmética"));
    }

    #[test]
    fn repeated_execution_uses_one_latest_snapshot_for_tokens_and_rating() {
        let mut job = execution();
        job.reported_tokens = Some(Tokens {
            input_tokens: 100,
            ..Tokens::default()
        });
        job.feedback = Some(feedback(0.6, 2));
        let earlier = prompt(&job);
        job.reported_tokens = Some(Tokens {
            input_tokens: 150,
            ..Tokens::default()
        });
        job.feedback = Some(feedback(0.8, 3));
        let latest = prompt(&job);
        let stats = ConversationStats::from_prompts(vec![earlier, latest]);
        assert_eq!(stats.prompts.len(), 2);
        assert_eq!(stats.total_tokens, Some(150));
        assert_eq!(stats.measured, 1);
        assert_eq!(stats.rated, 1);
        assert_eq!(stats.score, Some(8.0));
        assert!(!stats.partial);
    }

    #[test]
    fn open_or_untimed_log_events_keep_the_measurement_partial() {
        let mut job = execution();
        job.metrics = Some(Metrics {
            events: 1,
            open_turns: 1,
            ..Metrics::default()
        });
        assert!(prompt(&job).partial);
        job.metrics.as_mut().unwrap().open_turns = 0;
        job.metrics.as_mut().unwrap().untimed_events = 1;
        assert!(prompt(&job).partial);
        job.metrics.as_mut().unwrap().untimed_events = 0;
        assert!(!prompt(&job).partial);
        assert!(PromptStats::from_execution(1, "Em andamento", false, Some(&job)).partial);
    }
}
