//! The store: what the memory keeps, on disk, for years.
//!
//! One folder, `memory\` in the state folder. Each month the store is
//! written in gets a segment, `2026-10.jsonl`, holding the verbatim text
//! of its chunks with what is known about them, one JSON line each,
//! appended and never rewritten. Beside it each model that embedded them
//! has a file of its own, `2026-10.<model>.vec`, holding one vector per
//! chunk. The text is the truth and the vectors are a cache: a new model
//! embeds the kept text again ([`Store::catch_up`]), the old model's
//! vectors serving until it is done, then [`Store::forget`] drops them.
//!
//! A chunk goes into the segment of the month it was written in, not the
//! month it speaks of, so a month that has ended never changes and is
//! safe to back up or read while the current one grows. A backfill of
//! old transcripts lands in the month it ran.
//!
//! Every file opens with its format version. A file from a newer
//! Horadric (left behind by a reload that rolled back) is skipped when
//! read and refused when written, never misread. `tests/formats.rs`
//! reads a file of every earlier version.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The text segments' format. Raising it means `tests/formats.rs` gets
/// a file of the old version to keep reading.
pub const TEXT_VERSION: u32 = 1;
/// The vector files' format, counted apart from the text's.
pub const VECTOR_VERSION: u8 = 1;

const TEXT_FORMAT: &str = "horadric memory";
const VECTOR_MAGIC: &[u8; 5] = b"HMVEC";

/// A month, which names a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Month {
    pub year: u16,
    pub month: u8,
}

impl Month {
    /// The month a Unix time falls in, in UTC.
    pub fn of(secs: u64) -> Month {
        let (year, month) = civil((secs / 86_400) as i64);
        Month {
            year: year as u16,
            month: month as u8,
        }
    }

    /// `2026-10` back into a month.
    pub fn parse(s: &str) -> Option<Month> {
        let (y, m) = s.split_once('-')?;
        if y.len() != 4 || m.len() != 2 {
            return None;
        }
        let month: u8 = m.parse().ok()?;
        (1..=12).contains(&month).then_some(Month {
            year: y.parse().ok()?,
            month,
        })
    }
}

impl fmt::Display for Month {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}", self.year, self.month)
    }
}

/// Year and month of a day counted from 1970-01-01, after Howard
/// Hinnant's `civil_from_days`.
fn civil(days: i64) -> (i64, u64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u64;
    (yoe + era * 400 + i64::from(m <= 2), m)
}

/// Where a chunk is kept: its segment and its line there. It never
/// changes, so vectors and notes can point at it for years.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChunkId {
    pub month: Month,
    pub n: u32,
}

impl fmt::Display for ChunkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.month, self.n)
    }
}

impl ChunkId {
    /// `2026-10#12` back into an id.
    pub fn parse(s: &str) -> Option<ChunkId> {
        let (m, n) = s.split_once('#')?;
        Some(ChunkId {
            month: Month::parse(m)?,
            n: n.parse().ok()?,
        })
    }
}

/// What a chunk is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Note,
    Quest,
    Dialogue,
    Code,
    /// A kind a newer Horadric added. Kept and searched, never written.
    #[serde(other)]
    Other,
}

/// A file a chunk spoke of, with its hash then, so a later change to the
/// file can mark the chunk as maybe stale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMark {
    pub path: String,
    pub hash: u64,
}

/// A passage kept word for word, with what is known about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    /// When it was said or written, in Unix seconds.
    pub time: u64,
    pub project: String,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileMark>,
    /// The lore note this is a copy of, so recall finds a note's vector.
    /// Added after version 1 shipped: a build that does not know it
    /// skips it, so the format version stays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<u64>,
    pub text: String,
}

#[derive(Serialize, Deserialize)]
struct TextHeader {
    format: String,
    version: u32,
}

#[derive(Default)]
struct Segment {
    /// By line: `None` for a line that could not be read, so the lines
    /// after it keep their ids.
    chunks: Vec<Option<Chunk>>,
    /// How far the file has been read: always the end of a whole line.
    read_to: u64,
    /// Written by a newer Horadric.
    foreign: bool,
}

#[derive(Default)]
struct VectorFile {
    read_to: u64,
    foreign: bool,
}

/// One model's vectors, across every month.
struct Model {
    dims: usize,
    rows: HashMap<ChunkId, Vec<i8>>,
    files: BTreeMap<Month, VectorFile>,
}

pub struct Store {
    dir: PathBuf,
    segments: BTreeMap<Month, Segment>,
    models: BTreeMap<String, Model>,
    /// Vector files whose header could not be read at all, by month and
    /// tag: never written over, since a newer Horadric may have made them.
    unreadable: BTreeSet<(Month, String)>,
}

impl Store {
    /// Opens the store in `dir`, making the folder if it is not there,
    /// and reads all of it.
    pub fn open(dir: &Path) -> io::Result<Store> {
        fs::create_dir_all(dir)?;
        let mut store = Store {
            dir: dir.to_path_buf(),
            segments: BTreeMap::new(),
            models: BTreeMap::new(),
            unreadable: BTreeSet::new(),
        };
        store.refresh()?;
        Ok(store)
    }

    /// Reads what other writers added since the last read: new months,
    /// new lines, new vectors.
    pub fn refresh(&mut self) -> io::Result<()> {
        let mut found: Vec<(Month, Option<String>)> = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let name = entry?.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Some(m) = name.strip_suffix(".jsonl").and_then(Month::parse) {
                found.push((m, None));
            } else if let Some(rest) = name.strip_suffix(".vec") {
                if let Some((m, file_tag)) = rest.split_once('.') {
                    if let Some(m) = Month::parse(m) {
                        found.push((m, Some(file_tag.to_string())));
                    }
                }
            }
        }
        self.drop_replaced_vectors(&found);
        // Text first, so vectors find the chunks they belong to.
        found.sort_by_key(|(m, tag)| (tag.is_some(), *m));
        for (month, tag) in found {
            match tag {
                None => self.read_segment(month)?,
                Some(file_tag) => self.read_vectors(month, &file_tag)?,
            }
        }
        Ok(())
    }

    /// Forgets what was read of vector files another writer deleted (a
    /// model forgotten) or cut short, so they are read again from the
    /// start and never written past their end.
    fn drop_replaced_vectors(&mut self, found: &[(Month, Option<String>)]) {
        let is_found = |month: &Month, tag: &str| {
            found
                .iter()
                .any(|(m, t)| m == month && t.as_deref() == Some(tag))
        };
        let dir = self.dir.clone();
        for (tag, model) in &mut self.models {
            let mut gone = Vec::new();
            model.files.retain(|month, file| {
                let len =
                    fs::metadata(dir.join(format!("{month}.{tag}.vec"))).map_or(0, |m| m.len());
                let keep = is_found(month, tag) && len >= file.read_to;
                if !keep {
                    gone.push(*month);
                }
                keep
            });
            model.rows.retain(|id, _| !gone.contains(&id.month));
        }
        self.models.retain(|_, m| !m.files.is_empty());
        self.unreadable.retain(|(m, t)| is_found(m, t));
    }

    fn segment_path(&self, month: Month) -> PathBuf {
        self.dir.join(format!("{month}.jsonl"))
    }

    fn vector_path(&self, month: Month, tag: &str) -> PathBuf {
        self.dir.join(format!("{month}.{}.vec", file_tag(tag)))
    }

    fn read_segment(&mut self, month: Month) -> io::Result<()> {
        let path = self.segment_path(month);
        let seg = self.segments.entry(month).or_default();
        if seg.foreign {
            return Ok(());
        }
        let Some(bytes) = read_from(&path, seg.read_to)? else {
            return Ok(());
        };
        let whole = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        let mut lines = bytes[..whole].split(|&b| b == b'\n');
        lines.next_back();
        for line in lines {
            if seg.read_to == 0 {
                // No header this store reads: a newer one, or not its file.
                let header: Option<TextHeader> = serde_json::from_slice(line).ok();
                if !header.is_some_and(|h| h.format == TEXT_FORMAT && h.version <= TEXT_VERSION) {
                    seg.foreign = true;
                    return Ok(());
                }
                seg.read_to += line.len() as u64 + 1;
                continue;
            }
            seg.chunks.push(serde_json::from_slice(line).ok());
            seg.read_to += line.len() as u64 + 1;
        }
        Ok(())
    }

    fn read_vectors(&mut self, month: Month, file_tag: &str) -> io::Result<()> {
        let path = self.dir.join(format!("{month}.{file_tag}.vec"));
        if self.unreadable.contains(&(month, file_tag.to_string())) {
            return Ok(());
        }
        let known = self
            .models
            .get(file_tag)
            .filter(|m| m.files.contains_key(&month))
            .map(|_| file_tag.to_string());
        let (tag, dims, start) = match known {
            Some(tag) => {
                let file = &self.models[&tag].files[&month];
                if file.foreign {
                    return Ok(());
                }
                (tag.clone(), self.models[&tag].dims, file.read_to)
            }
            None => match read_vector_header(&path)? {
                Header::Missing => return Ok(()),
                Header::Foreign(tag) => {
                    if tag.as_deref() != Some(file_tag) {
                        self.unreadable.insert((month, file_tag.to_string()));
                    } else if let Some(tag) = tag {
                        let model = self.models.entry(tag).or_insert_with(|| Model {
                            dims: 0,
                            rows: HashMap::new(),
                            files: BTreeMap::new(),
                        });
                        model.files.insert(
                            month,
                            VectorFile {
                                read_to: 0,
                                foreign: true,
                            },
                        );
                    }
                    return Ok(());
                }
                Header::Read { tag, dims, len } => {
                    let model = self.models.entry(tag.clone()).or_insert_with(|| Model {
                        dims,
                        rows: HashMap::new(),
                        files: BTreeMap::new(),
                    });
                    if model.dims == 0 {
                        model.dims = dims;
                    }
                    if model.dims != dims {
                        model.files.insert(
                            month,
                            VectorFile {
                                read_to: 0,
                                foreign: true,
                            },
                        );
                        return Ok(());
                    }
                    model.files.insert(
                        month,
                        VectorFile {
                            read_to: len,
                            foreign: false,
                        },
                    );
                    (tag, dims, len)
                }
            },
        };
        let Some(bytes) = read_from(&path, start)? else {
            return Ok(());
        };
        let size = 4 + dims;
        let model = self.models.get_mut(&tag).expect("model was just found");
        for row in bytes.chunks_exact(size) {
            let n = u32::from_le_bytes(row[..4].try_into().expect("four bytes"));
            let v = row[4..].iter().map(|&b| b as i8).collect();
            model.rows.insert(ChunkId { month, n }, v);
        }
        let file = model.files.get_mut(&month).expect("file was just added");
        file.read_to = start + (bytes.len() / size * size) as u64;
        Ok(())
    }

    /// Keeps `chunk`, in the segment of the month `now` falls in.
    pub fn add(&mut self, chunk: &Chunk, now: u64) -> io::Result<ChunkId> {
        if chunk.kind == Kind::Other {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "a chunk of no known kind",
            ));
        }
        let month = Month::of(now);
        let _lock = Lock::take(&self.dir.join("write.lock"))?;
        self.refresh()?;
        let path = self.segment_path(month);
        let seg = self.segments.entry(month).or_default();
        if seg.foreign {
            return Err(newer(&path));
        }
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)?;
        // A tail past the last whole line is a write that died half way.
        file.set_len(seg.read_to)?;
        file.seek(SeekFrom::End(0))?;
        let mut out = Vec::new();
        if seg.read_to == 0 {
            serde_json::to_writer(
                &mut out,
                &TextHeader {
                    format: TEXT_FORMAT.into(),
                    version: TEXT_VERSION,
                },
            )?;
            out.push(b'\n');
        }
        serde_json::to_writer(&mut out, chunk)?;
        out.push(b'\n');
        file.write_all(&out)?;
        file.sync_data()?;
        seg.read_to += out.len() as u64;
        seg.chunks.push(Some(chunk.clone()));
        Ok(ChunkId {
            month,
            n: seg.chunks.len() as u32 - 1,
        })
    }

    pub fn chunk(&self, id: ChunkId) -> Option<&Chunk> {
        self.segments
            .get(&id.month)?
            .chunks
            .get(id.n as usize)?
            .as_ref()
    }

    /// Every chunk that could be read, oldest segment first.
    pub fn chunks(&self) -> impl Iterator<Item = (ChunkId, &Chunk)> {
        self.segments.iter().flat_map(|(&month, seg)| {
            seg.chunks
                .iter()
                .enumerate()
                .filter_map(move |(n, c)| Some((ChunkId { month, n: n as u32 }, c.as_ref()?)))
        })
    }

    pub fn len(&self) -> usize {
        self.chunks().count()
    }

    pub fn is_empty(&self) -> bool {
        self.chunks().next().is_none()
    }

    /// Keeps the vector `tag`'s model made of chunk `id`. A second one for
    /// the same chunk replaces the first.
    pub fn put_vector(&mut self, id: ChunkId, tag: &str, v: &[f32]) -> io::Result<()> {
        if v.is_empty() || v.len() > u16::MAX as usize || tag.is_empty() {
            return Err(io::Error::new(ErrorKind::InvalidInput, "no vector or tag"));
        }
        // A tag is its own file name, so two tags never share a file.
        if file_tag(tag) != tag {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                format!("{tag} is not a tag: letters, digits, -, _ and @ only"),
            ));
        }
        let _lock = Lock::take(&self.dir.join("write.lock"))?;
        self.refresh()?;
        let path = self.vector_path(id.month, tag);
        if self.unreadable.contains(&(id.month, tag.to_string())) {
            return Err(newer(&path));
        }
        let model = self.models.entry(tag.to_string()).or_insert_with(|| Model {
            dims: v.len(),
            rows: HashMap::new(),
            files: BTreeMap::new(),
        });
        if model.dims == 0 {
            // Seen only in files a newer Horadric wrote.
            model.dims = v.len();
        }
        if model.dims != v.len() {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                format!("{tag} has {} numbers, not {}", model.dims, v.len()),
            ));
        }
        let file_state = model.files.entry(id.month).or_default();
        if file_state.foreign {
            return Err(newer(&path));
        }
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)?;
        let mut out = Vec::new();
        if file_state.read_to == 0 {
            file.set_len(0)?;
            out.extend_from_slice(VECTOR_MAGIC);
            out.push(VECTOR_VERSION);
            out.extend_from_slice(&(v.len() as u16).to_le_bytes());
            out.extend_from_slice(&(tag.len() as u16).to_le_bytes());
            out.extend_from_slice(tag.as_bytes());
        } else {
            file.set_len(file_state.read_to)?;
        }
        file.seek(SeekFrom::End(0))?;
        let q = quantize(v);
        out.extend_from_slice(&id.n.to_le_bytes());
        out.extend(q.iter().map(|&x| x as u8));
        file.write_all(&out)?;
        file.sync_data()?;
        file_state.read_to += out.len() as u64;
        model.rows.insert(id, q);
        Ok(())
    }

    /// The models that have vectors here, each with how many chunks it
    /// covers.
    pub fn models(&self) -> Vec<(&str, usize)> {
        self.models
            .iter()
            .map(|(tag, m)| (tag.as_str(), m.rows.len()))
            .collect()
    }

    /// The chunks `tag`'s model has not embedded yet, oldest first,
    /// leaving out months whose vector file a newer Horadric wrote: they
    /// cannot be added to, and would hold up every month after them.
    pub fn missing(&self, tag: &str) -> Vec<ChunkId> {
        let model = self.models.get(tag);
        let blocked = |month: &Month| {
            self.unreadable.contains(&(*month, tag.to_string()))
                || model.is_some_and(|m| m.files.get(month).is_some_and(|f| f.foreign))
        };
        self.chunks()
            .map(|(id, _)| id)
            .filter(|id| !blocked(&id.month))
            .filter(|id| !model.is_some_and(|m| m.rows.contains_key(id)))
            .collect()
    }

    /// Embeds up to `most` of the chunks `tag`'s model has not, with
    /// `embed`, and keeps the vectors. Run again until it returns 0; each
    /// call is short, so the background work it is done in can stop
    /// between them.
    pub fn catch_up(
        &mut self,
        tag: &str,
        most: usize,
        mut embed: impl FnMut(&str) -> Result<Vec<f32>, String>,
    ) -> Result<usize, String> {
        let todo: Vec<ChunkId> = self.missing(tag).into_iter().take(most).collect();
        for &id in &todo {
            let text = &self.chunk(id).expect("missing lists kept chunks").text;
            let v = embed(text)?;
            self.put_vector(id, tag, &v).map_err(|e| e.to_string())?;
        }
        Ok(todo.len())
    }

    /// Deletes `tag`'s vectors, once a newer model has caught up.
    pub fn forget(&mut self, tag: &str) -> io::Result<()> {
        let _lock = Lock::take(&self.dir.join("write.lock"))?;
        self.refresh()?;
        if let Some(model) = self.models.remove(tag) {
            for month in model.files.keys() {
                match fs::remove_file(self.vector_path(*month, tag)) {
                    Err(e) if e.kind() != ErrorKind::NotFound => return Err(e),
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// The `k` chunks whose `tag` vectors are closest to `query`, closest
    /// first, with their cosine. Every vector is scored: at the sizes the
    /// plan expects that is milliseconds.
    pub fn nearest(&self, tag: &str, query: &[f32], k: usize) -> Vec<(ChunkId, f32)> {
        let Some(model) = self.models.get(tag) else {
            return Vec::new();
        };
        if model.dims != query.len() {
            return Vec::new();
        }
        let mut scored: Vec<(ChunkId, f32)> = model
            .rows
            .iter()
            .filter(|(id, _)| self.chunk(**id).is_some())
            .map(|(&id, v)| (id, dot(query, v)))
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        scored.truncate(k);
        scored
    }

    /// The cosine of chunk `id`'s `tag` vector and `query`, None when it
    /// has none yet.
    pub fn cosine(&self, tag: &str, id: ChunkId, query: &[f32]) -> Option<f32> {
        let model = self.models.get(tag)?;
        (model.dims == query.len()).then_some(())?;
        Some(dot(query, model.rows.get(&id)?))
    }

    /// The months whose files a newer Horadric wrote and this one skips.
    pub fn foreign(&self) -> BTreeSet<Month> {
        let text = self
            .segments
            .iter()
            .filter(|(_, s)| s.foreign)
            .map(|(m, _)| *m);
        let vectors = self
            .models
            .values()
            .flat_map(|m| m.files.iter().filter(|(_, f)| f.foreign).map(|(m, _)| *m));
        let unreadable = self.unreadable.iter().map(|(m, _)| *m);
        text.chain(vectors).chain(unreadable).collect()
    }
}

fn newer(path: &Path) -> io::Error {
    io::Error::new(
        ErrorKind::InvalidData,
        format!("{} was written by a newer Horadric", path.display()),
    )
}

/// The bytes of `path` from `start` on, or `None` when it is not there.
fn read_from(path: &Path, start: u64) -> io::Result<Option<Vec<u8>>> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

enum Header {
    Missing,
    /// Not readable here, with the model's tag when it could be told.
    Foreign(Option<String>),
    Read {
        tag: String,
        dims: usize,
        len: u64,
    },
}

fn read_vector_header(path: &Path) -> io::Result<Header> {
    let Some(bytes) = read_from(path, 0)? else {
        return Ok(Header::Missing);
    };
    if bytes.len() < 10 || &bytes[..5] != VECTOR_MAGIC {
        // Empty or torn before its header was whole: nothing kept yet.
        return Ok(
            if bytes.len() < 10 && VECTOR_MAGIC.starts_with(&bytes[..bytes.len().min(5)]) {
                Header::Missing
            } else {
                Header::Foreign(None)
            },
        );
    }
    let version = bytes[5];
    let dims = u16::from_le_bytes([bytes[6], bytes[7]]) as usize;
    let tag_len = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
    let Some(tag) = bytes.get(10..10 + tag_len) else {
        return Ok(Header::Missing);
    };
    let tag = String::from_utf8_lossy(tag).into_owned();
    if version > VECTOR_VERSION || dims == 0 {
        return Ok(Header::Foreign(Some(tag)));
    }
    Ok(Header::Read {
        tag,
        dims,
        len: (10 + tag_len) as u64,
    })
}

/// A model's tag as a file name can hold it. The header keeps the tag
/// itself.
fn file_tag(tag: &str) -> String {
    tag.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_@".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// A unit vector as bytes: a quarter of the size, and the order of the
/// best matches all but unchanged.
fn quantize(v: &[f32]) -> Vec<i8> {
    v.iter()
        .map(|x| (x * 127.0).round().clamp(-127.0, 127.0) as i8)
        .collect()
}

fn dot(query: &[f32], v: &[i8]) -> f32 {
    query
        .iter()
        .zip(v)
        .map(|(q, &x)| q * f32::from(x))
        .sum::<f32>()
        / 127.0
}

/// A lock file, as `horadric-hooks` guards the quest log and the lore
/// with: held while the file exists, taken as left by a dead process
/// after three seconds.
struct Lock(PathBuf);

impl Lock {
    fn take(path: &Path) -> io::Result<Lock> {
        let wait = Duration::from_secs(3);
        let step = Duration::from_millis(20);
        let mut waited = Duration::ZERO;
        loop {
            match OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(_) => return Ok(Lock(path.to_path_buf())),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    if waited >= wait {
                        let _ = fs::remove_file(path);
                        waited = Duration::ZERO;
                    }
                    thread::sleep(step);
                    waited += step;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-16 12:00 UTC.
    const OCT: u64 = 1_792_152_000;
    /// 2026-11-02 09:00 UTC.
    const NOV: u64 = 1_793_610_000;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("horadric-memory-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn chunk(text: &str) -> Chunk {
        Chunk {
            time: OCT,
            project: "horadric".into(),
            kind: Kind::Dialogue,
            session: Some("s1".into()),
            quest: None,
            commit: None,
            files: vec![FileMark {
                path: "src/lib.rs".into(),
                hash: 7,
            }],
            note: None,
            text: text.into(),
        }
    }

    fn unit(v: &[f32]) -> Vec<f32> {
        let len = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / len).collect()
    }

    #[test]
    fn months_come_from_unix_time() {
        assert_eq!(Month::of(0).to_string(), "1970-01");
        assert_eq!(Month::of(OCT).to_string(), "2026-10");
        assert_eq!(Month::of(NOV).to_string(), "2026-11");
        // 2024-02-29 23:59:59 and the second after it.
        assert_eq!(Month::of(1_709_251_199).to_string(), "2024-02");
        assert_eq!(Month::of(1_709_251_200).to_string(), "2024-03");
        assert_eq!(Month::parse("2026-10"), Some(Month::of(OCT)));
        assert_eq!(Month::parse("2026-13"), None);
        assert_eq!(Month::parse("26-10"), None);
    }

    #[test]
    fn ids_read_back() {
        let id = ChunkId {
            month: Month::of(OCT),
            n: 12,
        };
        assert_eq!(id.to_string(), "2026-10#12");
        assert_eq!(ChunkId::parse("2026-10#12"), Some(id));
        assert_eq!(ChunkId::parse("2026-10"), None);
    }

    #[test]
    fn chunks_go_into_the_month_they_are_written_in_and_come_back() {
        let dir = temp("months");
        let mut store = Store::open(&dir).unwrap();
        assert!(store.is_empty());
        let a = store.add(&chunk("the first"), OCT).unwrap();
        let b = store.add(&chunk("the second"), OCT).unwrap();
        let c = store.add(&chunk("a month on"), NOV).unwrap();
        assert_eq!(
            (a.to_string(), b.to_string()),
            ("2026-10#0".into(), "2026-10#1".into())
        );
        assert_eq!(c.to_string(), "2026-11#0");
        assert!(dir.join("2026-10.jsonl").is_file() && dir.join("2026-11.jsonl").is_file());

        let again = Store::open(&dir).unwrap();
        assert_eq!(again.len(), 3);
        assert_eq!(again.chunk(b), Some(&chunk("the second")));
        let texts: Vec<&str> = again.chunks().map(|(_, c)| c.text.as_str()).collect();
        assert_eq!(texts, ["the first", "the second", "a month on"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_second_writer_sees_the_first_ones_chunks() {
        let dir = temp("two");
        let mut one = Store::open(&dir).unwrap();
        let mut two = Store::open(&dir).unwrap();
        one.add(&chunk("from one"), OCT).unwrap();
        let id = two.add(&chunk("from two"), OCT).unwrap();
        assert_eq!(id.n, 1);
        one.refresh().unwrap();
        assert_eq!(one.chunk(id).unwrap().text, "from two");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_torn_last_line_is_ignored_and_then_cut() {
        let dir = temp("torn");
        let mut store = Store::open(&dir).unwrap();
        store.add(&chunk("whole"), OCT).unwrap();
        let path = dir.join("2026-10.jsonl");
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"time\":1,\"proj").unwrap();
        drop(f);

        let mut store = Store::open(&dir).unwrap();
        assert_eq!(store.len(), 1);
        let id = store.add(&chunk("after"), OCT).unwrap();
        assert_eq!(id.n, 1);
        let again = Store::open(&dir).unwrap();
        assert_eq!(again.chunk(id).unwrap().text, "after");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_bad_line_keeps_the_ids_after_it() {
        let dir = temp("bad");
        let mut store = Store::open(&dir).unwrap();
        store.add(&chunk("first"), OCT).unwrap();
        let path = dir.join("2026-10.jsonl");
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"not json\n").unwrap();
        drop(f);
        let mut store = Store::open(&dir).unwrap();
        let id = store.add(&chunk("third"), OCT).unwrap();
        assert_eq!(id.n, 2);
        assert_eq!(store.len(), 2);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_segment_from_a_newer_horadric_is_skipped_and_not_written() {
        let dir = temp("newer");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("2026-10.jsonl"),
            "{\"format\":\"horadric memory\",\"version\":99}\n{\"anything\":1}\n",
        )
        .unwrap();
        let mut store = Store::open(&dir).unwrap();
        assert!(store.is_empty());
        assert_eq!(store.foreign(), BTreeSet::from([Month::of(OCT)]));
        assert!(store.add(&chunk("no"), OCT).is_err());
        assert!(store.add(&chunk("yes"), NOV).is_ok());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn vectors_are_kept_per_model_and_found_by_meaning() {
        let dir = temp("vectors");
        let mut store = Store::open(&dir).unwrap();
        let a = store.add(&chunk("north"), OCT).unwrap();
        let b = store.add(&chunk("east"), OCT).unwrap();
        let c = store.add(&chunk("north east"), NOV).unwrap();
        store.put_vector(a, "m@1", &[1.0, 0.0]).unwrap();
        store.put_vector(b, "m@1", &[0.0, 1.0]).unwrap();
        store.put_vector(c, "m@1", &unit(&[1.0, 1.0])).unwrap();
        assert!(store.put_vector(a, "m@1", &[1.0, 0.0, 0.0]).is_err());

        let again = Store::open(&dir).unwrap();
        assert_eq!(again.models(), [("m@1", 3)]);
        let found = again.nearest("m@1", &unit(&[1.0, 0.2]), 2);
        assert_eq!(found.iter().map(|f| f.0).collect::<Vec<_>>(), [a, c]);
        assert!((found[0].1 - unit(&[1.0, 0.2])[0]).abs() < 0.01);
        assert!(again.nearest("other", &[1.0, 0.0], 2).is_empty());
        assert!(dir.join("2026-10.m@1.vec").is_file());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_new_model_catches_up_from_the_kept_text_and_the_old_is_forgotten() {
        let dir = temp("catch-up");
        let mut store = Store::open(&dir).unwrap();
        for word in ["one", "two", "three"] {
            let id = store.add(&chunk(word), OCT).unwrap();
            store.put_vector(id, "old", &[1.0, 0.0]).unwrap();
        }
        assert_eq!(store.missing("new").len(), 3);
        let embed = |t: &str| Ok(unit(&[t.len() as f32, 1.0, 0.5]));
        assert_eq!(store.catch_up("new", 2, embed), Ok(2));
        assert_eq!(store.missing("new").len(), 1);
        assert_eq!(store.catch_up("new", 2, embed), Ok(1));
        assert_eq!(store.catch_up("new", 2, embed), Ok(0));
        assert_eq!(store.models(), [("new", 3), ("old", 3)]);

        store.forget("old").unwrap();
        assert!(!dir.join("2026-10.old.vec").exists());
        let again = Store::open(&dir).unwrap();
        assert_eq!(again.models(), [("new", 3)]);
        assert!(again.missing("new").is_empty());

        let failing = |_: &str| Err::<Vec<f32>, _>("no model".to_string());
        assert!(Store::open(&dir)
            .unwrap()
            .catch_up("third", 5, failing)
            .is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_torn_vector_is_ignored_and_then_cut() {
        let dir = temp("torn-vec");
        let mut store = Store::open(&dir).unwrap();
        let a = store.add(&chunk("a"), OCT).unwrap();
        let b = store.add(&chunk("b"), OCT).unwrap();
        store.put_vector(a, "m", &[1.0, 0.0]).unwrap();
        let path = dir.join("2026-10.m.vec");
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&[1, 0, 0]).unwrap();
        drop(f);
        let mut store = Store::open(&dir).unwrap();
        assert_eq!(store.missing("m"), [b]);
        store.put_vector(b, "m", &[0.0, 1.0]).unwrap();
        assert!(Store::open(&dir).unwrap().missing("m").is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_vector_file_from_a_newer_horadric_holds_up_nothing() {
        let dir = temp("newer-vec");
        let mut store = Store::open(&dir).unwrap();
        let old = store.add(&chunk("old"), OCT).unwrap();
        let new = store.add(&chunk("new"), NOV).unwrap();
        let mut v2 = b"HMVEC".to_vec();
        v2.extend([2, 2, 0, 1, 0, b'm', 0, 0, 0, 0, 1, 1]);
        fs::write(dir.join("2026-10.m.vec"), &v2).unwrap();
        fs::write(dir.join("2026-10.n.vec"), b"something else entirely").unwrap();

        let mut store = Store::open(&dir).unwrap();
        assert_eq!(store.foreign(), BTreeSet::from([Month::of(OCT)]));
        assert_eq!(store.missing("m"), [new]);
        assert_eq!(store.missing("n"), [new]);
        assert!(store.put_vector(old, "m", &[1.0, 0.0]).is_err());
        assert!(store.put_vector(old, "n", &[1.0, 0.0]).is_err());
        let embed = |_: &str| Ok(vec![0.0, 1.0]);
        assert_eq!(store.catch_up("m", 5, embed), Ok(1));
        assert_eq!(store.catch_up("n", 5, embed), Ok(1));
        assert_eq!(fs::read(dir.join("2026-10.m.vec")).unwrap(), v2);
        assert_eq!(
            fs::read(dir.join("2026-10.n.vec")).unwrap(),
            b"something else entirely"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_model_forgotten_by_another_writer_is_forgotten_here_too() {
        let dir = temp("forgotten");
        let mut one = Store::open(&dir).unwrap();
        let a = one.add(&chunk("a"), OCT).unwrap();
        one.put_vector(a, "m", &[1.0, 0.0]).unwrap();
        let mut two = Store::open(&dir).unwrap();
        two.forget("m").unwrap();
        one.put_vector(a, "m", &[0.0, 1.0]).unwrap();
        let again = Store::open(&dir).unwrap();
        assert_eq!(again.models(), [("m", 1)]);
        assert_eq!(again.nearest("m", &[0.0, 1.0], 1)[0].0, a);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_tag_must_be_its_own_file_name() {
        let dir = temp("tags");
        let mut store = Store::open(&dir).unwrap();
        let a = store.add(&chunk("a"), OCT).unwrap();
        assert!(store.put_vector(a, "a/b", &[1.0]).is_err());
        assert!(store.put_vector(a, "a_b", &[1.0]).is_ok());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_tag_becomes_a_file_name() {
        assert_eq!(
            file_tag("all-MiniLM-L6-v2@1110a243"),
            "all-MiniLM-L6-v2@1110a243"
        );
        assert_eq!(file_tag("a/b:c.d"), "a_b_c_d");
    }

    #[test]
    fn quantizing_keeps_the_cosine_close() {
        let a = unit(&[0.3, -0.7, 0.2, 0.9]);
        let b = unit(&[0.1, -0.5, 0.6, 0.4]);
        let exact: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
        assert!((dot(&a, &quantize(&b)) - exact).abs() < 0.01);
        assert_eq!(quantize(&[1.0, -1.0, 2.0]), [127, -127, 127]);
    }
}
