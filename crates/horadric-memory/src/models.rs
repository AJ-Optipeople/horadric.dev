//! The model's weights, fetched once from Hugging Face into the state
//! folder's `models\` and checked by hash, never shipped in the
//! installer. See docs/plan/memory.md, part 1.
//!
//! The download itself is passed in: WinHTTP lives in `horadric-ui` and
//! curl on the Mac, and neither belongs in this crate.

use std::fs;
use std::path::{Path, PathBuf};

use crate::Kind;

/// One file of a pinned model.
pub struct File {
    pub name: &'static str,
    /// Lower case, as `hex` writes it.
    pub sha256: &'static str,
    pub size: usize,
}

/// A model's files at one commit of its Hugging Face repository, so what
/// is fetched is what the yardstick measured.
pub struct Pinned {
    pub repo: &'static str,
    pub commit: &'static str,
    pub files: &'static [File],
}

const MINILM: Pinned = Pinned {
    repo: "sentence-transformers/all-MiniLM-L6-v2",
    commit: "1110a243fdf4706b3f48f1d95db1a4f5529b4d41",
    files: &[
        File {
            name: "config.json",
            sha256: "953f9c0d463486b10a6871cc2fd59f223b2c70184f49815e7efbcab5d8908b41",
            size: 612,
        },
        File {
            name: "tokenizer.json",
            sha256: "be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037",
            size: 466_247,
        },
        File {
            name: "model.safetensors",
            sha256: "53aa51172d142c89d9012cce15ae4d6cc0ca6895895114379cacb4fab128d9db",
            size: 90_868_376,
        },
    ],
};

/// Written beside the files once all of them matched their hashes, so a
/// later start checks sizes only instead of hashing 90 MB again.
const MARK: &str = "verified";

impl Kind {
    /// The pinned files, for the models Horadric fetches by itself. The
    /// others were measured once and are fetched by hand.
    pub fn pinned(self) -> Option<&'static Pinned> {
        match self {
            Kind::MiniLm => Some(&MINILM),
            Kind::EmbeddingGemma | Kind::BgeSmall => None,
        }
    }
}

pub fn url(pinned: &Pinned, file: &File) -> String {
    format!(
        "https://huggingface.co/{}/resolve/{}/{}",
        pinned.repo, pinned.commit, file.name
    )
}

/// The folder `kind` loads from under `models`, fetching whatever is
/// missing or does not match its hash. `fetch(url, limit)` returns the
/// body of `url`, at most `limit` bytes. Nothing lands under its own
/// name unless its hash matched, so a broken download is fetched again
/// next time rather than loaded.
pub fn ensure(
    kind: Kind,
    models: &Path,
    fetch: impl Fn(&str, usize) -> Result<Vec<u8>, String>,
) -> Result<PathBuf, String> {
    let pinned = kind
        .pinned()
        .ok_or_else(|| format!("{} is not fetched by Horadric", kind.folder()))?;
    let dir = models.join(kind.folder());
    fill(pinned, &dir, fetch)?;
    Ok(dir)
}

fn fill(
    pinned: &Pinned,
    dir: &Path,
    fetch: impl Fn(&str, usize) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    if is_verified(pinned, dir) {
        return Ok(());
    }
    fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    for file in pinned.files {
        let path = dir.join(file.name);
        if fs::read(&path).is_ok_and(|bytes| hex(&sha256(&bytes)) == file.sha256) {
            continue;
        }
        let body = fetch(&url(pinned, file), file.size)?;
        let got = hex(&sha256(&body));
        if got != file.sha256 {
            return Err(format!(
                "{} from {} has SHA-256 {got}, not {}",
                file.name, pinned.repo, file.sha256
            ));
        }
        // Another process may be fetching the same file: each writes its
        // own part and the rename replaces whole files only.
        let part = dir.join(format!("{}.{}.part", file.name, std::process::id()));
        let placed = fs::write(&part, &body).and_then(|()| fs::rename(&part, &path));
        if let Err(e) = placed {
            let _ = fs::remove_file(&part);
            // Windows refuses to replace a file another process has
            // mapped; if that process put the right one there, it is done.
            if !fs::read(&path).is_ok_and(|bytes| hex(&sha256(&bytes)) == file.sha256) {
                return Err(format!("cannot put {} in place: {e}", file.name));
            }
        }
    }
    fs::write(dir.join(MARK), pinned.commit)
        .map_err(|e| format!("cannot mark {} verified: {e}", dir.display()))
}

fn is_verified(pinned: &Pinned, dir: &Path) -> bool {
    fs::read_to_string(dir.join(MARK)).is_ok_and(|c| c.trim() == pinned.commit)
        && pinned
            .files
            .iter()
            .all(|f| fs::metadata(dir.join(f.name)).is_ok_and(|m| m.len() == f.size as u64))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 (FIPS 180-4), written out rather than taken from a crate:
/// this is its one use here, and it is sixty lines.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let (whole, rest) = data.as_chunks::<64>();
    let mut tail = rest.to_vec();
    tail.push(0x80);
    while tail.len() % 64 != 56 {
        tail.push(0);
    }
    tail.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in whole.iter().chain(tail.as_chunks::<64>().0) {
        let mut w = [0u32; 64];
        for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 32];
    for (o, x) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        *o = x.to_be_bytes();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn sum(data: &[u8]) -> String {
        hex(&sha256(data))
    }

    #[test]
    fn sha256_matches_the_standard_vectors() {
        assert_eq!(
            sum(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sum(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sum(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            sum(&vec![b'a'; 1_000_000]),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn sha256_pads_lengths_around_a_block() {
        // 55 bytes fit the length in one block, 56 do not, 64 is exactly one.
        assert_eq!(
            sum(&[b'a'; 55]),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        assert_eq!(
            sum(&[b'a'; 56]),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
        assert_eq!(
            sum(&[b'a'; 64]),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }

    #[test]
    fn minilm_is_pinned_and_the_others_are_not() {
        let p = Kind::MiniLm.pinned().unwrap();
        assert_eq!(
            url(p, &p.files[0]),
            "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/1110a243fdf4706b3f48f1d95db1a4f5529b4d41/config.json"
        );
        assert!(p.files.iter().all(|f| f.sha256.len() == 64));
        assert!(Kind::EmbeddingGemma.pinned().is_none());
        assert!(Kind::BgeSmall.pinned().is_none());
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("horadric-models-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn pinned_bytes() -> Vec<Vec<u8>> {
        // Small stand ins: the test checks the plumbing, not the model.
        MINILM
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| vec![i as u8; f.size.min(1000 + i)])
            .collect()
    }

    /// A pinned table made from `bytes`, so the test needs neither the
    /// network nor 90 MB.
    fn fake_pinned(bytes: &[Vec<u8>]) -> Pinned {
        let files: Vec<File> = MINILM
            .files
            .iter()
            .zip(bytes)
            .map(|(f, b)| File {
                name: f.name,
                sha256: Box::leak(sum(b).into_boxed_str()),
                size: b.len(),
            })
            .collect();
        Pinned {
            repo: MINILM.repo,
            commit: MINILM.commit,
            files: Box::leak(files.into_boxed_slice()),
        }
    }

    #[test]
    fn fetches_once_then_trusts_the_mark() {
        let bytes = pinned_bytes();
        let pinned = fake_pinned(&bytes);
        let dir = scratch("once");
        let asked = RefCell::new(Vec::new());
        let fetch = |u: &str, _: usize| {
            asked.borrow_mut().push(u.to_string());
            let i = pinned
                .files
                .iter()
                .position(|f| u.ends_with(f.name))
                .unwrap();
            Ok(bytes[i].clone())
        };
        fill(&pinned, &dir, fetch).unwrap();
        assert_eq!(asked.borrow().len(), 3);
        fill(&pinned, &dir, fetch).unwrap();
        assert_eq!(
            asked.borrow().len(),
            3,
            "a verified folder is not fetched again"
        );

        // A file cut short is fetched again, and only that one.
        fs::write(dir.join("tokenizer.json"), b"x").unwrap();
        fill(&pinned, &dir, fetch).unwrap();
        assert_eq!(asked.borrow().len(), 4);
        assert!(asked.borrow()[3].ends_with("tokenizer.json"));
        assert_eq!(fs::read(dir.join("tokenizer.json")).unwrap(), bytes[1]);

        // A mark from another commit means the files are checked again.
        fs::write(dir.join(MARK), "an older commit").unwrap();
        fill(&pinned, &dir, fetch).unwrap();
        assert_eq!(asked.borrow().len(), 4, "the files on disk still match");
        assert_eq!(fs::read_to_string(dir.join(MARK)).unwrap(), pinned.commit);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_wrong_hash_is_refused_and_not_marked() {
        let dir = scratch("wrong");
        let err = ensure(Kind::MiniLm, &dir, |_, _| Ok(b"not the model".to_vec())).unwrap_err();
        assert!(err.contains("config.json"), "{err}");
        let folder = dir.join(Kind::MiniLm.folder());
        assert!(!folder.join("config.json").exists());
        assert!(!folder.join(MARK).exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn models_horadric_does_not_fetch_are_refused() {
        let err = ensure(Kind::BgeSmall, Path::new("unused"), |_, _| unreachable!()).unwrap_err();
        assert!(err.contains("bge-small"), "{err}");
    }
}
