use std::env;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;


use crate::session::{Session, SessionError};

pub const PRIMARY_SESSION_EXTENSION: &str = "jsonl";
pub const LEGACY_SESSION_EXTENSION: &str = "json";
pub const LATEST_SESSION_REFERENCE: &str = "latest";

const SESSION_REFERENCE_ALIASES: &[&str] = &[LATEST_SESSION_REFERENCE, "last", "recent"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHandle {
    pub id: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedSessionSummary {
    pub id: String,
    pub path: PathBuf,
    pub modified_epoch_millis: u128,
    pub message_count: usize,
    pub parent_session_id: Option<String>,
    pub branch_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedManagedSession {
    pub handle: SessionHandle,
    pub session: Session,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkedManagedSession {
    pub parent_session_id: String,
    pub handle: SessionHandle,
    pub session: Session,
    pub branch_name: Option<String>,
}

#[derive(Debug)]
pub enum SessionControlError {
    Io(std::io::Error),
    Session(SessionError),
    Format(String),
}

impl Display for SessionControlError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Session(error) => write!(f, "{error}"),
            Self::Format(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for SessionControlError {}

impl From<std::io::Error> for SessionControlError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<SessionError> for SessionControlError {
    fn from(value: SessionError) -> Self {
        Self::Session(value)
    }
}

pub fn sessions_dir() -> Result<PathBuf, SessionControlError> {
    managed_sessions_dir_for(env::current_dir()?)
}

pub fn managed_sessions_dir_for(
    base_dir: impl AsRef<Path>,
) -> Result<PathBuf, SessionControlError> {
    let path = base_dir.as_ref().join(".claw").join("sessions");
    fs::create_dir_all(&path)?;
    Ok(path)
}

pub fn create_managed_session_handle(
    session_id: &str,
) -> Result<SessionHandle, SessionControlError> {
    create_managed_session_handle_for(env::current_dir()?, session_id)
}

pub fn create_managed_session_handle_for(
    base_dir: impl AsRef<Path>,
    session_id: &str,
) -> Result<SessionHandle, SessionControlError> {
    let id = session_id.to_string();
    let path =
        managed_sessions_dir_for(base_dir)?.join(format!("{id}.{PRIMARY_SESSION_EXTENSION}"));
    Ok(SessionHandle { id, path })
}

pub fn resolve_session_reference(reference: &str) -> Result<SessionHandle, SessionControlError> {
    resolve_session_reference_for(env::current_dir()?, reference)
}

pub fn resolve_session_reference_for(
    base_dir: impl AsRef<Path>,
    reference: &str,
) -> Result<SessionHandle, SessionControlError> {
    let base_dir = base_dir.as_ref();
    if is_session_reference_alias(reference) {
        let latest = latest_managed_session_for(base_dir)?;
        return Ok(SessionHandle {
            id: latest.id,
            path: latest.path,
        });
    }

    let direct = PathBuf::from(reference);
    let candidate = if direct.is_absolute() {
        direct.clone()
    } else {
        base_dir.join(&direct)
    };
    let looks_like_path = direct.extension().is_some() || direct.components().count() > 1;
    let path = if candidate.exists() {
        candidate
    } else if looks_like_path {
        return Err(SessionControlError::Format(
            format_missing_session_reference(reference),
        ));
    } else {
        resolve_managed_session_path_for(base_dir, reference)?
    };

    Ok(SessionHandle {
        id: session_id_from_path(&path).unwrap_or_else(|| reference.to_string()),
        path,
    })
}

pub fn resolve_managed_session_path(session_id: &str) -> Result<PathBuf, SessionControlError> {
    resolve_managed_session_path_for(env::current_dir()?, session_id)
}

pub fn resolve_managed_session_path_for(
    base_dir: impl AsRef<Path>,
    session_id: &str,
) -> Result<PathBuf, SessionControlError> {
    let directory = managed_sessions_dir_for(base_dir)?;
    for extension in [PRIMARY_SESSION_EXTENSION, LEGACY_SESSION_EXTENSION] {
        let path = directory.join(format!("{session_id}.{extension}"));
        if path.exists() {
            return Ok(path);
        }
    }
    Err(SessionControlError::Format(
        format_missing_session_reference(session_id),
    ))
}

#[must_use]
pub fn is_managed_session_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|extension| {
            extension == PRIMARY_SESSION_EXTENSION || extension == LEGACY_SESSION_EXTENSION
        })
}

pub fn list_managed_sessions() -> Result<Vec<ManagedSessionSummary>, SessionControlError> {
    list_managed_sessions_for(env::current_dir()?)
}

pub fn list_managed_sessions_for(
    base_dir: impl AsRef<Path>,
) -> Result<Vec<ManagedSessionSummary>, SessionControlError> {
    let mut sessions = Vec::new();
    for entry in fs::read_dir(managed_sessions_dir_for(base_dir)?)? {
        let entry = entry?;
        let path = entry.path();
        if !is_managed_session_file(&path) {
            continue;
        }
        let metadata = entry.metadata()?;
        let modified_epoch_millis = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        let (id, message_count, parent_session_id, branch_name) =
            match Session::load_from_path(&path) {
                Ok(session) => {
                    let parent_session_id = session
                        .fork
                        .as_ref()
                        .map(|fork| fork.parent_session_id.clone());
                    let branch_name = session
                        .fork
                        .as_ref()
                        .and_then(|fork| fork.branch_name.clone());
                    (
                        session.session_id,
                        session.messages.len(),
                        parent_session_id,
                        branch_name,
                    )
                }
                Err(_) => (
                    path.file_stem()
                        .and_then(|value| value.to_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    0,
                    None,
                    None,
                ),
            };
        sessions.push(ManagedSessionSummary {
            id,
            path,
            modified_epoch_millis,
            message_count,
            parent_session_id,
            branch_name,
        });
    }
    sessions.sort_by(|left, right| {
        right
            .modified_epoch_millis
            .cmp(&left.modified_epoch_millis)
            .then_with(|| right.id.cmp(&left.id))
    });
    Ok(sessions)
}

pub fn latest_managed_session() -> Result<ManagedSessionSummary, SessionControlError> {
    latest_managed_session_for(env::current_dir()?)
}

pub fn latest_managed_session_for(
    base_dir: impl AsRef<Path>,
) -> Result<ManagedSessionSummary, SessionControlError> {
    list_managed_sessions_for(base_dir)?
        .into_iter()
        .next()
        .ok_or_else(|| SessionControlError::Format(format_no_managed_sessions()))
}

pub fn load_managed_session(reference: &str) -> Result<LoadedManagedSession, SessionControlError> {
    load_managed_session_for(env::current_dir()?, reference)
}

pub fn load_managed_session_for(
    base_dir: impl AsRef<Path>,
    reference: &str,
) -> Result<LoadedManagedSession, SessionControlError> {
    let handle = resolve_session_reference_for(base_dir, reference)?;
    let session = Session::load_from_path(&handle.path)?;
    Ok(LoadedManagedSession {
        handle: SessionHandle {
            id: session.session_id.clone(),
            path: handle.path,
        },
        session,
    })
}

pub fn fork_managed_session(
    session: &Session,
    branch_name: Option<String>,
) -> Result<ForkedManagedSession, SessionControlError> {
    fork_managed_session_for(env::current_dir()?, session, branch_name)
}

pub fn fork_managed_session_for(
    base_dir: impl AsRef<Path>,
    session: &Session,
    branch_name: Option<String>,
) -> Result<ForkedManagedSession, SessionControlError> {
    let parent_session_id = session.session_id.clone();
    let forked = session.fork(branch_name);
    let handle = create_managed_session_handle_for(base_dir, &forked.session_id)?;
    let branch_name = forked
        .fork
        .as_ref()
        .and_then(|fork| fork.branch_name.clone());
    let forked = forked.with_persistence_path(handle.path.clone());
    forked.save_to_path(&handle.path)?;
    Ok(ForkedManagedSession {
        parent_session_id,
        handle,
        session: forked,
        branch_name,
    })
}

#[must_use]
pub fn is_session_reference_alias(reference: &str) -> bool {
    SESSION_REFERENCE_ALIASES
        .iter()
        .any(|alias| reference.eq_ignore_ascii_case(alias))
}

fn session_id_from_path(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|value| value.to_str())
        .and_then(|name| {
            name.strip_suffix(&format!(".{PRIMARY_SESSION_EXTENSION}"))
                .or_else(|| name.strip_suffix(&format!(".{LEGACY_SESSION_EXTENSION}")))
        })
        .map(ToOwned::to_owned)
}

fn format_missing_session_reference(reference: &str) -> String {
    format!(
        "session not found: {reference}\nHint: managed sessions live in .claw/sessions/. Try `{LATEST_SESSION_REFERENCE}` for the most recent session or `/session list` in the REPL."
    )
}

fn format_no_managed_sessions() -> String {
    format!(
        "no managed sessions found in .claw/sessions/\nStart `claw` to create a session, then rerun with `--resume {LATEST_SESSION_REFERENCE}`."
    )
}

#[cfg(test)]
mod tests {
    use super::{
        create_managed_session_handle_for, fork_managed_session_for, is_session_reference_alias,
        list_managed_sessions_for, load_managed_session_for, resolve_session_reference_for,
        ManagedSessionSummary, LATEST_SESSION_REFERENCE,
    };
    use crate::session::Session;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("runtime-session-control-{nanos}"))
    }

    fn persist_session(root: &Path, text: &str) -> Session {
        let mut session = Session::new();
        session
            .push_user_text(text)
            .expect("session message should save");
        let handle = create_managed_session_handle_for(root, &session.session_id)
            .expect("managed session handle should build");
        let session = session.with_persistence_path(handle.path.clone());
        session
            .save_to_path(&handle.path)
            .expect("session should persist");
        session
    }

    fn wait_for_next_millisecond() {
        let start = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_millis();
        while SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_millis()
            <= start
        {}
    }

    fn summary_by_id<'a>(
        summaries: &'a [ManagedSessionSummary],
        id: &str,
    ) -> &'a ManagedSessionSummary {
        summaries
            .iter()
            .find(|summary| summary.id == id)
            .expect("session summary should exist")
    }

    #[test]
    fn creates_and_lists_managed_sessions() {
        // given
        let root = temp_dir();
        fs::create_dir_all(&root).expect("root dir should exist");
        let older = persist_session(&root, "older session");
        wait_for_next_millisecond();
        let newer = persist_session(&root, "newer session");

        // when
        let sessions = list_managed_sessions_for(&root).expect("managed sessions should list");

        // then
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, newer.session_id);
        assert_eq!(summary_by_id(&sessions, &older.session_id).message_count, 1);
        assert_eq!(summary_by_id(&sessions, &newer.session_id).message_count, 1);
        fs::remove_dir_all(root).expect("temp dir should clean up");
    }

    #[test]
    fn resolves_latest_alias_and_loads_session_from_workspace_root() {
        // given
        let root = temp_dir();
        fs::create_dir_all(&root).expect("root dir should exist");
        let older = persist_session(&root, "older session");
        wait_for_next_millisecond();
        let newer = persist_session(&root, "newer session");

        // when
        let handle = resolve_session_reference_for(&root, LATEST_SESSION_REFERENCE)
            .expect("latest alias should resolve");
        let loaded = load_managed_session_for(&root, "recent")
            .expect("recent alias should load the latest session");

        // then
        assert_eq!(handle.id, newer.session_id);
        assert_eq!(loaded.handle.id, newer.session_id);
        assert_eq!(loaded.session.messages.len(), 1);
        assert_ne!(loaded.handle.id, older.session_id);
        assert!(is_session_reference_alias("last"));
        fs::remove_dir_all(root).expect("temp dir should clean up");
    }

    #[test]
    fn forks_session_into_managed_storage_with_lineage() {
        // given
        let root = temp_dir();
        fs::create_dir_all(&root).expect("root dir should exist");
        let source = persist_session(&root, "parent session");

        // when
        let forked = fork_managed_session_for(&root, &source, Some("incident-review".to_string()))
            .expect("session should fork");
        let sessions = list_managed_sessions_for(&root).expect("managed sessions should list");
        let summary = summary_by_id(&sessions, &forked.handle.id);

        // then
        assert_eq!(forked.parent_session_id, source.session_id);
        assert_eq!(forked.branch_name.as_deref(), Some("incident-review"));
        assert_eq!(
            summary.parent_session_id.as_deref(),
            Some(source.session_id.as_str())
        );
        assert_eq!(summary.branch_name.as_deref(), Some("incident-review"));
        assert_eq!(
            forked.session.persistence_path(),
            Some(forked.handle.path.as_path())
        );
        fs::remove_dir_all(root).expect("temp dir should clean up");
    }
}
