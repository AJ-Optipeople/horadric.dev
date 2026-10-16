//! A backup in Google Drive: `Horadric backup\<machine>\`, one Drive file
//! per backed up file, named by its relative path (`lore/x.json.hbk`
//! stays one name with a slash in it, so a machine's backup is one
//! folder and one listing). With the `drive.file` scope Horadric sees only
//! what it made itself. See docs/plan/backup.md, Google Drive.

use std::collections::HashMap;
use std::sync::Mutex;

use horadric_core::google::{self, DriveFile};

use crate::google::Session;
use crate::net::{self, Answer};

/// Larger than any month's segment will grow.
const LIMIT: usize = 1 << 30;

pub struct DriveVault {
    session: Mutex<Session>,
    folder: String,
    /// Each file's Drive id by name, from the last listing and since.
    ids: Mutex<HashMap<String, String>>,
}

impl DriveVault {
    /// This `machine`'s backup, its folders made on the first use.
    pub fn open(machine: &str) -> Result<DriveVault, String> {
        let mut session = Session::new()?;
        let top =
            folder(&mut session, google::BACKUP_FOLDER, "root", true)?.ok_or("no backup folder")?;
        let folder = folder(&mut session, machine, &top, true)?.ok_or("no machine folder")?;
        let vault = DriveVault {
            session: Mutex::new(session),
            folder,
            ids: Mutex::new(HashMap::new()),
        };
        vault.list()?;
        Ok(vault)
    }

    /// The machines with a backup in this Drive, for a restore on a new PC.
    pub fn machines() -> Result<Vec<String>, String> {
        let mut session = Session::new()?;
        let Some(top) = folder(&mut session, google::BACKUP_FOLDER, "root", false)? else {
            return Ok(Vec::new());
        };
        let mut names: Vec<String> = list_all(&mut session, &google::children_query(&top, true))?
            .into_iter()
            .map(|f| f.name)
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }

    /// Every file in this machine's backup folder, by relative name.
    pub fn list(&self) -> Result<Vec<String>, String> {
        let files = list_all(
            &mut self.session.lock().unwrap(),
            &google::children_query(&self.folder, false),
        )?;
        let mut ids = self.ids.lock().unwrap();
        ids.clear();
        let mut names = Vec::new();
        for f in files {
            names.push(f.name.clone());
            ids.insert(f.name, f.id);
        }
        names.sort();
        names.dedup();
        Ok(names)
    }

    pub fn get(&self, name: &str) -> Result<Vec<u8>, String> {
        let id = self
            .id(name)
            .ok_or_else(|| format!("{name} is not in the backup"))?;
        let url = format!("{}/{id}?alt=media", google::FILES_URL);
        let answer = call(&mut self.session.lock().unwrap(), "GET", &url, &[], &[])?;
        ok(&answer, name)?;
        Ok(answer.body)
    }

    /// Writes or replaces, as one resumable upload: Drive's simple upload
    /// stops at 5 MB, and a month's segment passes that.
    pub fn put(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let length = bytes.len().to_string();
        let mut session = self.session.lock().unwrap();
        let headers = [
            ("Content-Type", "application/json; charset=UTF-8"),
            ("X-Upload-Content-Type", "application/octet-stream"),
            ("X-Upload-Content-Length", length.as_str()),
        ];
        let start = match self.id(name) {
            Some(id) => call(
                &mut session,
                "PATCH",
                &format!("{}/{id}?uploadType=resumable", google::UPLOAD_URL),
                &headers,
                b"{}",
            )?,
            None => call(
                &mut session,
                "POST",
                &format!("{}?uploadType=resumable", google::UPLOAD_URL),
                &headers,
                google::metadata(name, &self.folder, false).as_bytes(),
            )?,
        };
        ok(&start, name)?;
        let upload = start
            .location
            .ok_or_else(|| format!("Drive gave no upload address for {name}"))?;
        // The session URL carries its own authority, so no token here.
        let done = net::request(
            "PUT",
            &upload,
            &[("Content-Type", "application/octet-stream")],
            bytes,
            1 << 20,
        )?;
        ok(&done, name)?;
        if let Some(id) = google::parse_id(&done.body) {
            self.ids.lock().unwrap().insert(name.to_string(), id);
        }
        Ok(())
    }

    pub fn delete(&self, name: &str) -> Result<(), String> {
        let Some(id) = self.id(name) else {
            return Ok(());
        };
        let url = format!("{}/{id}", google::FILES_URL);
        let answer = call(&mut self.session.lock().unwrap(), "DELETE", &url, &[], &[])?;
        if answer.status != 404 {
            ok(&answer, name)?;
        }
        self.ids.lock().unwrap().remove(name);
        Ok(())
    }

    fn id(&self, name: &str) -> Option<String> {
        self.ids.lock().unwrap().get(name).cloned()
    }
}

/// The folder `name` in `parent`, made when missing and `make`.
fn folder(
    session: &mut Session,
    name: &str,
    parent: &str,
    make: bool,
) -> Result<Option<String>, String> {
    if let Some(f) = list_all(session, &google::folder_query(name, parent))?
        .into_iter()
        .next()
    {
        return Ok(Some(f.id));
    }
    if !make {
        return Ok(None);
    }
    let answer = call(
        session,
        "POST",
        google::FILES_URL,
        &[("Content-Type", "application/json; charset=UTF-8")],
        google::metadata(name, parent, true).as_bytes(),
    )?;
    ok(&answer, name)?;
    google::parse_id(&answer.body)
        .map(Some)
        .ok_or_else(|| format!("Drive made {name} but gave no id"))
}

fn list_all(session: &mut Session, query: &str) -> Result<Vec<DriveFile>, String> {
    let mut all = Vec::new();
    let mut page = None;
    loop {
        let url = google::list_url(query, page.as_deref());
        let answer = call(session, "GET", &url, &[], &[])?;
        ok(&answer, "the file list")?;
        let (files, next) = google::parse_files(&answer.body)?;
        all.extend(files);
        match next {
            Some(n) => page = Some(n),
            None => return Ok(all),
        }
    }
}

/// One authorised call, once more with a fresh token if Drive says the
/// old one died early.
fn call(
    session: &mut Session,
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<Answer, String> {
    let send = |token: &str| {
        let bearer = format!("Bearer {token}");
        let mut all = vec![("Authorization", bearer.as_str())];
        all.extend_from_slice(headers);
        net::request(method, url, &all, body, LIMIT)
    };
    let answer = send(session.token()?)?;
    if answer.status != 401 {
        return Ok(answer);
    }
    session.refresh()?;
    send(session.token()?)
}

fn ok(answer: &Answer, what: &str) -> Result<(), String> {
    if (200..300).contains(&answer.status) {
        return Ok(());
    }
    let why = serde_json::from_slice::<serde_json::Value>(&answer.body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_default();
    Err(
        format!("Drive answered {} for {what}. {why}", answer.status)
            .trim_end()
            .to_string(),
    )
}
