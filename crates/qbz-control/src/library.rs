//! Experimental Orbit library inspection. This host advertises only the
//! capabilities it serves; inspection never grants playback/audio authority.
//! Track DTOs contain metadata and an instance-scoped identity, never host paths
//! or media-server credentials. The existing daemon catalog API is unchanged.
use std::io::Cursor;
use std::sync::Arc;

use qbz_library::service::LibraryService;
use serde::{Deserialize, Serialize};
use tiny_http::{Request, Response};
use tokio::sync::broadcast;

use crate::{err_json, json, HttpHost};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibraryInfo {
    pub protocol: u32,
    /// Rotates whenever the listener starts or the host profile changes.
    pub instance: String,
    pub name: String,
    pub capabilities: Vec<String>,
    pub sources: Vec<String>,
    pub tracks: u64,
    pub folders: usize,
    pub revision: u64,
    pub scanning: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibraryTrack {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_secs: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibraryPage {
    pub instance: String,
    pub revision: u64,
    pub tracks: Vec<LibraryTrack>,
    pub has_more: bool,
}

pub struct LibraryEndpoint {
    service: Arc<LibraryService>,
    name: String,
    instance: String,
    management: bool,
}

impl LibraryEndpoint {
    /// Host supplies a service and explicitly chooses whether to offer administration.
    /// The enclosing HttpHost applies its existing Origin/bearer policy first.
    pub fn new(service: Arc<LibraryService>, name: String, management: bool) -> Self {
        Self {
            service,
            name,
            management,
            instance: uuid::Uuid::new_v4().to_string(),
        }
    }

    fn info(&self) -> Response<Cursor<Vec<u8>>> {
        let result = self
            .service
            .store()
            .read(|db| Ok((db.get_stats(true)?, db.get_folders_with_metadata()?.len())));
        let (tracks, folders) = match result {
            Ok(Some((stats, folders))) => (stats.track_count, folders),
            Ok(None) => (0, 0),
            Err(error) => return failed(error),
        };
        let snapshot = self.service.snapshot();
        json(
            200,
            serde_json::to_value(LibraryInfo {
                protocol: PROTOCOL_VERSION,
                instance: self.instance.clone(),
                name: self.name.clone(),
                capabilities: {
                    let mut caps = vec![
                        "library.files.inspect".into(),
                        "library.files.search".into(),
                    ];
                    if self.management {
                        caps.push("library.files.manage".into());
                    }
                    caps
                },
                sources: vec!["files".into()],
                tracks: tracks as u64,
                folders,
                revision: snapshot.revision,
                scanning: snapshot.progress.running,
            })
            .expect("library info is JSON"),
        )
    }

    fn search(&self, query: &str) -> Response<Cursor<Vec<u8>>> {
        let params: std::collections::HashMap<_, _> = query
            .split('&')
            .filter_map(|part| {
                let (key, value) = part.split_once('=')?;
                Some((
                    key,
                    urlencoding::decode(&value.replace('+', " "))
                        .ok()?
                        .into_owned(),
                ))
            })
            .collect();
        if params.get("instance") != Some(&self.instance) {
            return err_json(
                409,
                "host_changed",
                "the library host changed",
                "verify the host again",
            );
        }
        let limit = match params.get("limit").map(|v| v.parse::<u64>()) {
            None => 25,
            Some(Ok(n @ 1..=100)) => n,
            _ => {
                return err_json(
                    400,
                    "invalid_limit",
                    "limit must be between 1 and 100",
                    "use a bounded page",
                )
            }
        };
        let offset = match params.get("offset").map(|v| v.parse::<u64>()) {
            None => 0,
            Some(Ok(n @ 0..=100_000)) => n,
            _ => {
                return err_json(
                    400,
                    "invalid_offset",
                    "invalid library offset",
                    "start a new search",
                )
            }
        };
        let q = params.get("q").map(String::as_str).unwrap_or("").trim();
        if q.len() > 1024 {
            return err_json(
                400,
                "invalid_query",
                "query is too long",
                "shorten the search",
            );
        }
        let rows =
            if q.chars().count() < 2 {
                Vec::new()
            } else {
                match self.service.store().read(|db| {
                    db.search_with_filter_page(q, offset, limit + 1, true, false, "default")
                }) {
                    Ok(rows) => rows.unwrap_or_default(),
                    Err(error) => return failed(error),
                }
            };
        let has_more = rows.len() > limit as usize;
        let tracks = rows
            .into_iter()
            .take(limit as usize)
            .map(|t| LibraryTrack {
                id: t.id.to_string(),
                title: t.title,
                artist: t.artist,
                album: t.album,
                duration_secs: t.duration_secs,
            })
            .collect();
        json(
            200,
            serde_json::to_value(LibraryPage {
                instance: self.instance.clone(),
                revision: self.service.snapshot().revision,
                tracks,
                has_more,
            })
            .expect("library page is JSON"),
        )
    }
}

impl LibraryEndpoint {
    pub fn route(&self, request: &mut Request) -> Response<Cursor<Vec<u8>>> {
        if self.service.snapshot().closed {
            return err_json(
                409,
                "host_closed",
                "this library is no longer active",
                "verify the host again",
            );
        }
        let url = request.url().to_owned();
        let (path, query) = url.split_once('?').unwrap_or((&url, ""));
        match (request.method().as_str(), path) {
            ("GET", "/api/orbit/library") => self.info(),
            ("GET", "/api/orbit/library/search") => self.search(query),
            (
                _,
                "/api/orbit/library/folders"
                | "/api/orbit/library/scan"
                | "/api/orbit/library/cancel"
                | "/api/orbit/library/jobs",
            ) if self.management => self.manage(request, path, query),
            _ => err_json(
                404,
                "not_supported",
                "this host does not offer that operation",
                "check its advertised capabilities",
            ),
        }
    }
}

fn failed(error: qbz_library::LibraryError) -> Response<Cursor<Vec<u8>>> {
    log::warn!("Orbit library query failed: {error}");
    err_json(
        500,
        "library_unavailable",
        "the host library could not be read",
        "check the host's library settings",
    )
}

/// Standalone opt-in Qt laboratory host. A daemon embeds LibraryEndpoint in
/// its existing HTTP host instead of opening another server or runtime.
pub struct LibraryHost {
    endpoint: LibraryEndpoint,
    token: String,
    events: broadcast::Sender<qbz_models::CoreEvent>,
}
impl LibraryHost {
    pub fn new(service: Arc<LibraryService>, name: String, token: String) -> Self {
        Self::with_management(service, name, token, false)
    }
    pub fn with_management(
        service: Arc<LibraryService>,
        name: String,
        token: String,
        management: bool,
    ) -> Self {
        Self {
            endpoint: LibraryEndpoint::new(service, name, management),
            token,
            events: broadcast::channel(16).0,
        }
    }
}
impl HttpHost for LibraryHost {
    fn token(&self) -> Option<&str> {
        Some(&self.token)
    }
    fn events(&self) -> &broadcast::Sender<qbz_models::CoreEvent> {
        &self.events
    }
    fn route(&self, request: &mut Request) -> Response<Cursor<Vec<u8>>> {
        self.endpoint.route(request)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LibraryCommand {
    instance: String,
    revision: Option<u64>,
    path: Option<String>,
    folder_id: Option<i64>,
    job: Option<u64>,
}

impl LibraryEndpoint {
    fn manage(&self, request: &mut Request, path: &str, query: &str) -> Response<Cursor<Vec<u8>>> {
        let method = request.method().as_str();
        if method == "GET"
            && matches!(
                path,
                "/api/orbit/library/folders" | "/api/orbit/library/jobs"
            )
        {
            let instance = query.split('&').find_map(|p| p.strip_prefix("instance="));
            if instance != Some(self.instance.as_str()) {
                return changed();
            }
            return if path.ends_with("/jobs") {
                self.jobs(200)
            } else {
                self.folders()
            };
        }
        if method != "POST"
            || !matches!(
                path,
                "/api/orbit/library/folders"
                    | "/api/orbit/library/scan"
                    | "/api/orbit/library/cancel"
            )
        {
            return err_json(
                405,
                "method_not_allowed",
                "unsupported library method",
                "check the library contract",
            );
        }
        use std::io::Read;
        let mut bytes = Vec::new();
        if request
            .as_reader()
            .take(8193)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() > 8192
        {
            return err_json(
                400,
                "invalid_body",
                "invalid library command",
                "send a bounded JSON object",
            );
        }
        let Ok(command) = serde_json::from_slice::<LibraryCommand>(&bytes) else {
            return err_json(
                400,
                "invalid_body",
                "invalid library command",
                "send a valid JSON object",
            );
        };
        if command.instance != self.instance {
            return changed();
        }
        match path {
            "/api/orbit/library/folders" => {
                let (Some(path), Some(revision)) = (command.path, command.revision) else {
                    return err_json(
                        400,
                        "invalid_body",
                        "path and revision are required",
                        "read the host library first",
                    );
                };
                match self
                    .service
                    .register_folder(std::path::Path::new(&path), revision)
                {
                    Ok(_) => self.folders(),
                    Err(qbz_library::service::FolderError::Storage(e)) => failed(e),
                    Err(qbz_library::service::FolderError::InvalidPath) => err_json(
                        400,
                        "invalid_path",
                        "expected an existing absolute directory on the host",
                        "use the host's path, not the controller's path",
                    ),
                    Err(qbz_library::service::FolderError::Busy) => err_json(
                        409,
                        "library_busy",
                        "a library scan is running",
                        "wait for the scan or cancel it",
                    ),
                    Err(qbz_library::service::FolderError::Conflict) => err_json(
                        409,
                        "folder_conflict",
                        "folder overlaps an existing library root",
                        "use the existing root",
                    ),
                    Err(qbz_library::service::FolderError::Disabled) => err_json(
                        409,
                        "folder_disabled",
                        "folder is disabled",
                        "enable it in the host library settings",
                    ),
                    Err(qbz_library::service::FolderError::Closed) => changed(),
                    Err(qbz_library::service::FolderError::RevisionChanged) => err_json(
                        409,
                        "revision_changed",
                        "the library changed",
                        "refresh before trying again",
                    ),
                }
            }
            "/api/orbit/library/scan" => {
                if let Some(id) = command.folder_id {
                    match self.service.store().read(|db| db.get_folder_by_id(id)) {
                        Ok(Some(Some(folder))) if folder.enabled => {}
                        Ok(Some(Some(_))) => {
                            return err_json(
                                409,
                                "folder_disabled",
                                "folder is disabled",
                                "enable it in the host library settings",
                            )
                        }
                        Ok(_) => {
                            return err_json(
                                404,
                                "folder_not_found",
                                "folder does not exist on this host",
                                "refresh the folder list",
                            )
                        }
                        Err(e) => return failed(e),
                    }
                }
                match self.service.scan(command.folder_id) {
                    Ok(true) => self.jobs(202),
                    Ok(false) => changed(),
                    Err(e) => failed(e),
                }
            }
            _ => {
                let Some(job) = command.job else {
                    return err_json(
                        400,
                        "invalid_body",
                        "job is required",
                        "read the active scan first",
                    );
                };
                if self.service.cancel_job(job) {
                    self.jobs(202)
                } else {
                    err_json(
                        409,
                        "job_changed",
                        "that scan is no longer active",
                        "refresh the scan status",
                    )
                }
            }
        }
    }

    fn jobs(&self, status: u16) -> Response<Cursor<Vec<u8>>> {
        json(
            status,
            serde_json::json!({"instance": self.instance, "library": self.service.snapshot()}),
        )
    }

    fn folders(&self) -> Response<Cursor<Vec<u8>>> {
        match self
            .service
            .store()
            .read(|db| db.get_folders_with_metadata())
        {
            Ok(rows) => {
                // Paths are deliberate administrative data, never part of search
                // metadata. Serialize only what the folder settings need.
                let folders: Vec<_> = rows
                    .unwrap_or_default()
                    .into_iter()
                    .map(|f| serde_json::json!({"id":f.id, "path":f.path, "enabled":f.enabled}))
                    .collect();
                json(
                    200,
                    serde_json::json!({"instance": self.instance, "revision": self.service.snapshot().revision, "folders": folders}),
                )
            }
            Err(e) => failed(e),
        }
    }
}
fn changed() -> Response<Cursor<Vec<u8>>> {
    err_json(
        409,
        "host_changed",
        "the library host changed",
        "verify the host again",
    )
}
