use crate::{db::Db, model::*};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};
use walkdir::WalkDir;

#[derive(Debug, Default, Clone, Serialize)]
pub struct ImportReport {
    pub files_read: usize,
    pub files_unchanged: usize,
    pub new_events: usize,
    pub malformed_lines: usize,
    pub incomplete_lines: usize,
    pub errors: Vec<String>,
    pub synced_at: Option<DateTime<Utc>>,
}

pub fn sync(db: &mut Db, path: &Path) -> Result<ImportReport> {
    ensure!(
        path.exists(),
        "Pasta de sessões não encontrada: {}",
        path.display()
    );
    let mut report = ImportReport::default();
    for entry in WalkDir::new(path).follow_links(false) {
        let entry = match entry {
            Ok(v) => v,
            Err(e) => {
                report.errors.push(e.to_string());
                continue;
            }
        };
        if !entry.file_type().is_file() || entry.path().extension().is_none_or(|e| e != "jsonl") {
            continue;
        }
        let result = (|| -> Result<()> {
            let file_path = entry.path().canonicalize()?;
            let key = file_path.to_string_lossy();
            let metadata = file_path.metadata()?;
            let stamp = format!("{}:{:?}", metadata.len(), metadata.modified()?);
            if db.unchanged(&key, &stamp)? {
                report.files_unchanged += 1;
                return Ok(());
            }
            let (data, malformed, incomplete) = parse(BufReader::new(File::open(&file_path)?))?;
            report.new_events += db.ingest(&data)?;
            report.files_read += 1;
            report.malformed_lines += malformed;
            report.incomplete_lines += incomplete;
            let after = file_path.metadata()?;
            if malformed == 0
                && incomplete == 0
                && stamp == format!("{}:{:?}", after.len(), after.modified()?)
            {
                db.checkpoint(&key, &stamp)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            report
                .errors
                .push(format!("{}: {error}", entry.file_name().to_string_lossy()));
        }
    }
    report.synced_at = Some(Utc::now());
    Ok(report)
}

// Only metadata, counters and timing are retained. Prompts and tool output are discarded.
pub fn parse(reader: impl BufRead) -> Result<(Dataset, usize, usize)> {
    let mut data = Dataset::default();
    let mut malformed = 0;
    let mut incomplete = 0;
    let mut previous: Option<Tokens> = None;
    let mut model = "unknown".to_string();
    let mut effort = "unknown".to_string();
    let mut tier = "unknown".to_string();
    let mut current_turn: Option<String> = None;
    let mut turns = BTreeMap::<String, Turn>::new();
    for line in reader.split(b'\n') {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_slice(&line) {
            Ok(v) => v,
            Err(e) => {
                if e.is_eof() {
                    incomplete += 1;
                } else {
                    malformed += 1;
                }
                continue;
            }
        };
        let payload = &value["payload"];
        let kind = value["type"].as_str().unwrap_or_default();
        if kind == "session_meta" {
            // Fork files start with their own metadata, followed by copied parent history.
            if !data.sessions.is_empty() {
                continue;
            }
            let id = payload["id"]
                .as_str()
                .context("Métadados sem ID")?
                .to_string();
            let created_at =
                timestamp(&payload["timestamp"]).context("Metadados sem data válida")?;
            let spawn = &payload["source"]["subagent"]["thread_spawn"];
            let parent_id = spawn["parent_thread_id"].as_str().map(str::to_string);
            data.sessions.push(Session {
                id,
                parent_id,
                name: spawn["agent_path"]
                    .as_str()
                    .or(spawn["agent_nickname"].as_str())
                    .unwrap_or("Orquestrador")
                    .to_string(),
                project: payload["cwd"].as_str().unwrap_or_default().to_string(),
                provider: payload["model_provider"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string(),
                created_at,
                source: "codex-local".into(),
            });
            continue;
        }
        let Some(session) = data.sessions.first() else {
            continue;
        };
        let Some(at) = timestamp(&value["timestamp"]) else {
            continue;
        };
        let inherited = at < session.created_at;
        if kind == "turn_context" {
            // Inherited configuration can be useful, but never inherit an active turn.
            if let Some(s) = payload["model"].as_str() {
                model = s.into();
            }
            if let Some(s) = payload["effort"]
                .as_str()
                .or(payload["reasoning_effort"].as_str())
            {
                effort = s.into();
            }
            tier = payload["service_tier"].as_str().unwrap_or("unknown").into();
            if !inherited {
                current_turn = payload["turn_id"].as_str().map(str::to_string);
            }
        }
        if kind != "event_msg" {
            continue;
        }
        let event = payload["type"].as_str().unwrap_or_default();
        if event == "token_count" {
            let info = &payload["info"];
            let Ok(total) = serde_json::from_value::<Tokens>(info["total_token_usage"].clone())
            else {
                continue;
            };
            let last = serde_json::from_value::<Tokens>(info["last_token_usage"].clone()).ok();
            let delta = match previous {
                Some(prev) => total.delta(prev).or(last),
                None => last.or(Some(total)),
            };
            previous = Some(total);
            if inherited {
                continue;
            }
            let Some(tokens) = delta else {
                continue;
            };
            if !tokens.valid() {
                malformed += 1;
                continue;
            }
            if tokens.total() == 0 {
                continue;
            }
            let key = serde_json::to_vec(&(&session.id, at, total))?;
            let id = format!("codex:{:x}", Sha256::digest(key));
            data.usage.push(Usage {
                id,
                session_id: session.id.clone(),
                turn_id: current_turn.clone(),
                at,
                model: model.clone(),
                effort: effort.clone(),
                service_tier: tier.clone(),
                tokens,
            });
        } else if !inherited && matches!(event, "task_started" | "task_complete" | "turn_aborted") {
            let Some(id) = payload["turn_id"].as_str() else {
                continue;
            };
            let started = payload["started_at"]
                .as_i64()
                .and_then(|n| DateTime::from_timestamp(n, 0));
            if event == "task_started" {
                current_turn = Some(id.into());
                turns.entry(id.into()).or_insert(Turn {
                    id: id.into(),
                    session_id: session.id.clone(),
                    started_at: started.unwrap_or(at).max(session.created_at),
                    ended_at: None,
                    status: "running".into(),
                    ttft_ms: None,
                });
            } else {
                let begin = turns.get(id).map(|t| t.started_at).or(started).or_else(|| {
                    payload["duration_ms"]
                        .as_i64()
                        .filter(|n| *n >= 0)
                        .map(|n| at - chrono::Duration::milliseconds(n))
                });
                if let Some(begin) = begin.filter(|b| *b >= session.created_at && *b <= at) {
                    turns.insert(
                        id.into(),
                        Turn {
                            id: id.into(),
                            session_id: session.id.clone(),
                            started_at: begin,
                            ended_at: Some(at),
                            status: if event == "task_complete" {
                                "completed"
                            } else {
                                "aborted"
                            }
                            .into(),
                            ttft_ms: payload["time_to_first_token_ms"].as_u64(),
                        },
                    );
                }
            }
        }
    }
    ensure!(
        !data.sessions.is_empty(),
        "Arquivo sem session_meta compatível"
    );
    data.turns = turns.into_values().collect();
    Ok((data, malformed, incomplete))
}

fn timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
}
