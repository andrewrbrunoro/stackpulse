//! Read-only migration; the original database remains available as a backup.
use super::*;
use rusqlite::{Connection, OpenFlags, params};

impl ChatHistory {
    pub(super) fn migrate(&self, metrics: &Path) -> Result<usize> {
        let name = metrics
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("stackpulse.sqlite");
        let path = metrics.with_file_name(format!("{name}.chat.sqlite"));
        if !path.is_file() {
            return Ok(0);
        }
        let mut connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        let tx = connection.transaction()?;
        let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='chat_sessions')", [], |r| r.get(0))?;
        if !exists {
            return Ok(0);
        }
        let custom: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('chat_sessions') WHERE name='custom_title')", [], |r| r.get(0))?;
        let has_activity: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='chat_agent_activity')",
            [],
            |r| r.get(0),
        )?;
        let has_messages: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='chat_agent_messages')",
            [],
            |r| r.get(0),
        )?;
        let mut statement = tx.prepare(&format!(
            "SELECT id,title,created_at,updated_at,ended_at,owner_pid,{} FROM chat_sessions WHERE project=?1",
            if custom { "custom_title" } else { "0" }
        ))?;
        let mut rows = statement.query([self.project.to_string_lossy().as_ref()])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            if self.path(&id)?.exists() {
                continue;
            }
            if row.get::<_, Option<i64>>(5)?.is_some_and(process_alive) {
                self.warnings.borrow_mut().push("Uma conversa do banco antigo ainda está aberta. Feche o terminal antigo e reabra para migrá-la.".into());
                continue;
            }
            let _lock = self.lock(&id)?;
            if self.path(&id)?.exists() {
                continue;
            }
            let mut summary = SessionSummary {
                id: id.clone(),
                title: row.get(1)?,
                created_at: row.get::<_, String>(2)?.parse()?,
                updated_at: row.get::<_, String>(3)?.parse()?,
                ended_at: row
                    .get::<_, Option<String>>(4)?
                    .map(|s| s.parse())
                    .transpose()?,
                custom_title: row.get(6)?,
                turns: 0,
            };
            let mut turns = vec![];
            let mut activities = HashMap::new();
            let mut messages = HashMap::new();
            let mut statement = tx.prepare("SELECT id,prompt,response,status,profile,execution,started_at,ended_at FROM chat_turns WHERE session_id=?1 ORDER BY position,started_at,id")?;
            let mut rows = statement.query([&id])?;
            while let Some(row) = rows.next()? {
                let turn = StoredTurn {
                    id: row.get(0)?,
                    prompt: row.get(1)?,
                    response: serde_json::from_str(&row.get::<_, String>(2)?)?,
                    status: row.get(3)?,
                    profile: row.get(4)?,
                    execution: row
                        .get::<_, Option<String>>(5)?
                        .map(|s| serde_json::from_str(&s))
                        .transpose()?,
                    started_at: Some(row.get::<_, String>(6)?.parse()?),
                    ended_at: row
                        .get::<_, Option<String>>(7)?
                        .map(|s| s.parse())
                        .transpose()?,
                    source_events: vec![],
                };
                if has_activity {
                    use rusqlite::OptionalExtension;
                    let snapshot: Option<String> = tx
                        .query_row(
                            "SELECT snapshot FROM chat_agent_activity WHERE turn_id=?1",
                            [&turn.id],
                            |r| r.get(0),
                        )
                        .optional()?;
                    if let Some(snapshot) = snapshot {
                        activities.insert(turn.id.clone(), serde_json::from_str(&snapshot)?);
                    }
                }
                if has_messages {
                    let mut statement = tx.prepare("SELECT id,agent_id,redirect,message,status,detail FROM chat_agent_messages WHERE turn_id=?1 ORDER BY created_at,id")?;
                    let items = statement.query_map(params![turn.id], |r| {
                        Ok(AgentMessage {
                            id: r.get(0)?,
                            agent_id: r.get(1)?,
                            redirect: r.get(2)?,
                            message: r.get(3)?,
                            status: r.get(4)?,
                            detail: r.get(5)?,
                        })
                    })?;
                    messages.insert(
                        turn.id.clone(),
                        items.collect::<rusqlite::Result<Vec<_>>>()?,
                    );
                }
                turns.push(turn);
            }
            summary.turns = turns.len();
            let document = Document {
                session: StoredSession { summary, turns },
                activities,
                messages,
                origin: Some(Origin {
                    client: "stackpulse-sqlite".into(),
                    session_id: id,
                }),
            };
            validate_document(&document)?;
            self.write_new(&document)?;
            count += 1;
        }
        Ok(count)
    }
}

#[cfg(unix)]
fn process_alive(pid: i64) -> bool {
    if pid <= 0 || pid > i64::from(i32::MAX) {
        return false;
    }
    unsafe {
        libc::kill(pid as i32, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}
#[cfg(not(unix))]
fn process_alive(_pid: i64) -> bool {
    true
}
