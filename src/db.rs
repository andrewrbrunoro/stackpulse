use crate::model::*;
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use std::{collections::HashSet, path::Path};

pub struct Db(pub Connection);

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let db = Self(Connection::open(path)?);
        db.0.busy_timeout(std::time::Duration::from_secs(5))?;
        db.0.execute_batch(
            "PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY, data TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS usage (id TEXT PRIMARY KEY, data TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS turns (id TEXT PRIMARY KEY, data TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS annotations (id TEXT PRIMARY KEY, data TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS prices (id TEXT PRIMARY KEY, data TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS imports (path TEXT PRIMARY KEY, stamp TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS executions (id TEXT PRIMARY KEY, data TEXT NOT NULL);
            PRAGMA user_version=2;",
        )?;
        Ok(db)
    }

    pub fn ingest(&mut self, data: &Dataset) -> Result<usize> {
        validate(data)?;
        let tx = self.0.transaction()?;
        for session in &data.sessions {
            let old: Option<String> = tx
                .query_row(
                    "SELECT data FROM sessions WHERE id=?1",
                    [&session.id],
                    |r| r.get(0),
                )
                .optional()?;
            let mut session = session.clone();
            if let Some(old) = old {
                let old: Session = serde_json::from_str(&old)?;
                session.created_at = session.created_at.min(old.created_at);
            }
            put(&tx, "sessions", &session.id, &session)?;
        }
        let mut inserted = 0;
        for usage in &data.usage {
            inserted += tx.execute(
                "INSERT OR IGNORE INTO usage(id,data) VALUES (?1,?2)",
                params![usage.id, serde_json::to_string(usage)?],
            )?;
        }
        for turn in &data.turns {
            let key = format!("{}:{}", turn.session_id, turn.id);
            let old: Option<String> = tx
                .query_row("SELECT data FROM turns WHERE id=?1", [&key], |r| r.get(0))
                .optional()?;
            if let Some(old) = old {
                let old: Turn = serde_json::from_str(&old)?;
                // A stale file must not reopen a completed turn.
                if old.ended_at.is_some() && turn.ended_at.is_none() {
                    continue;
                }
            }
            put(&tx, "turns", &key, turn)?;
        }
        for annotation in &data.annotations {
            put(&tx, "annotations", &annotation.run_id, annotation)?;
        }
        for price in &data.prices {
            let key = serde_json::to_string(&(
                &price.provider,
                &price.model,
                &price.service_tier,
                price.effective_at,
            ))?;
            put(&tx, "prices", &key, price)?;
        }
        tx.commit()?;
        Ok(inserted)
    }

    pub fn snapshot(&self) -> Result<Dataset> {
        Ok(Dataset {
            sessions: self.read("sessions")?,
            usage: self.read("usage")?,
            turns: self.read("turns")?,
            annotations: self.read("annotations")?,
            prices: self.read("prices")?,
        })
    }

    pub fn save_execution(&mut self, execution: &crate::workflow::Execution) -> Result<()> {
        if let Some(feedback) = &execution.feedback {
            feedback.validate()?;
        }
        let tx = self.0.transaction()?;
        put(&tx, "executions", &execution.id, execution)?;
        tx.commit()?;
        Ok(())
    }

    pub fn executions(&self) -> Result<Vec<crate::workflow::Execution>> {
        self.read("executions")
    }

    fn read<T: DeserializeOwned>(&self, table: &str) -> Result<Vec<T>> {
        let mut stmt = self
            .0
            .prepare(&format!("SELECT data FROM {table} ORDER BY id"))?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|s| Ok(serde_json::from_str(&s?)?)).collect()
    }

    pub fn unchanged(&self, path: &str, stamp: &str) -> Result<bool> {
        let previous: Option<String> = self
            .0
            .query_row("SELECT stamp FROM imports WHERE path=?1", [path], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(previous.as_deref() == Some(stamp))
    }

    pub fn checkpoint(&self, path: &str, stamp: &str) -> Result<()> {
        self.0.execute("INSERT INTO imports(path,stamp) VALUES (?1,?2) ON CONFLICT(path) DO UPDATE SET stamp=excluded.stamp", params![path, stamp])?;
        Ok(())
    }
}

fn put(
    tx: &rusqlite::Transaction<'_>,
    table: &str,
    id: &str,
    value: &impl Serialize,
) -> Result<()> {
    tx.execute(&format!("INSERT INTO {table}(id,data) VALUES (?1,?2) ON CONFLICT(id) DO UPDATE SET data=excluded.data"), params![id, serde_json::to_string(value)?])?;
    Ok(())
}

pub fn validate(data: &Dataset) -> Result<()> {
    let sessions: HashSet<_> = data.sessions.iter().map(|s| s.id.as_str()).collect();
    ensure!(
        sessions.len() == data.sessions.len(),
        "IDs de sessão duplicados no lote"
    );
    for s in &data.sessions {
        ensure!(
            !s.id.is_empty() && s.id.len() <= 512,
            "ID de sessão inválido"
        );
        ensure!(
            s.parent_id.as_deref() != Some(&s.id),
            "Uma sessão não pode ser seu próprio pai"
        );
    }
    for u in &data.usage {
        ensure!(
            !u.id.is_empty() && !u.session_id.is_empty(),
            "ID de consumo inválido"
        );
        ensure!(u.tokens.valid(), "Contadores de tokens inconsistentes");
    }
    for t in &data.turns {
        ensure!(
            !t.id.is_empty() && !t.session_id.is_empty(),
            "ID de turno inválido"
        );
        ensure!(
            t.ended_at.is_none_or(|end| end >= t.started_at),
            "Fim do turno anterior ao início"
        );
    }
    for a in &data.annotations {
        ensure!(!a.run_id.is_empty(), "Execução obrigatória");
        ensure!(
            a.label.len() <= 200 && a.benchmark.len() <= 300,
            "Rótulo muito longo"
        );
        ensure!(
            a.quality
                .is_none_or(|q| q.is_finite() && (0.0..=1.0).contains(&q)),
            "Qualidade deve estar entre 0 e 1"
        );
    }
    for p in &data.prices {
        ensure!(
            !p.model.is_empty() && !p.provider.is_empty() && !p.source.trim().is_empty(),
            "Modelo, provedor e fonte da tarifa são obrigatórios"
        );
        ensure!(
            [
                p.input_per_million,
                p.cached_per_million,
                p.cache_write_per_million,
                p.output_per_million
            ]
            .iter()
            .all(|n| n.is_finite() && *n >= 0.0 && *n < 1e9),
            "Tarifa inválida"
        );
    }
    Ok(())
}
