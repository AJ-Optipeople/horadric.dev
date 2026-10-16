# Backup

Asked for on 2026-10-16. The memory ([memory.md](memory.md)) is kept on
this machine only, and in a few months losing it will hurt. The human
uses Google, not OneDrive, and wants one click: sign in with Google,
and the backup takes care of itself. Others (Dropbox, OneDrive) can
follow the same shape later; a plain folder stays for a NAS or a USB
disk.

## Decisions

- **Copies, never the live store in a synced folder.** Sync clients and
  files appended to fight, and two machines writing one memory would
  break it. Syncing a memory between machines is another project.
- **Text only.** The vectors are a cache and are rebuilt after a restore,
  and the models are fetched again. What is backed up: `memory\*.jsonl`
  (the monthly segments), `memory\horadrim.json`, `memory\transcripts.json`,
  `memory\day\`, and the lore notes `lore\*.json` (not `*.code.json`,
  which is rebuilt). About 100 MB a year.
- **Encrypted before it leaves.** AES-256-GCM through Windows CNG, as the
  Town Portal does (`horadric-ui/src/portal_crypto.rs`), with the key
  derived from a passphrase the human chooses (PBKDF2-SHA256 through
  CNG, 600,000 rounds, a random salt). The cloud only ever holds noise.
  The passphrase is kept in Windows Credential Manager so backups run
  unattended, and the Settings section says plainly that without it the
  backup cannot be read.
- **No new crate.** HTTPS through WinHTTP, which the updater already uses
  (`horadric-ui/src/net.rs`, GET only today, so POST, PATCH and DELETE
  are added there). Crypto through CNG. Tokens through Credential
  Manager. All in the `windows` crate.
- **Closed months once.** A month's segment never changes after the
  month ends, so it is uploaded once. The rest are uploaded again when
  their plaintext hash changes, at most once an hour.

## The format

A backup is a folder (on disk, or in Drive) named `Horadric backup`,
holding one subfolder per machine (the computer's name), so two PCs on
one Google account never overwrite each other. In it:

- `manifest.json`, in the clear: format version, the KDF and its salt
  and rounds, and per file its relative path, plaintext SHA-256, size,
  and when it was backed up. No content and no file names beyond the
  store's own (`2026-10.jsonl`, `lore/<project>-<hash>.json`).
- One blob per file, `<relative path>.<first 16 hex of its SHA-256>.hbk`
  (named for its content so a new version is put beside the old one,
  which goes only after the new manifest is in): 12 bytes of nonce, the
  ciphertext, its 16 byte tag, with the relative path as associated data
  so a blob cannot be swapped for another.

A wrong passphrase is caught on the first blob's tag, before anything is
written.

## The vault

Where a backup goes is a `Vault`:

```rust
pub trait Vault {
    /// Every file in this machine's backup folder, by relative name.
    fn list(&self) -> Result<Vec<String>, String>;
    fn get(&self, name: &str) -> Result<Vec<u8>, String>;
    /// Writes or replaces.
    fn put(&self, name: &str, bytes: &[u8]) -> Result<(), String>;
    fn delete(&self, name: &str) -> Result<(), String>;
}
```

Names use `/`. The folder vault and the Google Drive vault implement it;
the backup logic (what changed, encrypt, put, the manifest last so a
half finished backup is never the one a restore reads) is written once
against the trait and tested with an in memory vault.

## Google Drive

- **Sign in in the real browser.** Google refuses sign in inside an
  embedded web view, so the browser pane cannot be used: Horadric opens
  the default browser on Google's consent page and listens on
  `http://127.0.0.1:<free port>` for the answer (OAuth 2.0 for installed
  apps, with PKCE). The page the browser lands on says it can be closed.
- **The narrowest scope**, `https://www.googleapis.com/auth/drive.file`:
  Horadric sees only the files it made. It is not a sensitive scope, so
  no Google security review is needed and users see no "unverified app"
  warning. Plus `openid email` so the Settings section can show which
  account it is.
- **The refresh token** goes in Credential Manager, under a name that
  includes the state folder, so a dev instance has its own.
- **Uploads are resumable** (segments pass Drive's 5 MB simple upload
  limit), each file found by name and parent folder and replaced in
  place.
- **The OAuth client** is a "Desktop app" client in a Google Cloud
  project owned by the human. Its id and secret are compiled in; for an
  installed app Google treats the secret as public. They are in
  `crates/horadric-ui/src/google.rs` once the quest that builds sign in
  has them.

## Restore

`horadric backup restore` and a button in Settings: pick the machine's
backup (another machine's too, for a new PC), give the passphrase,
download and decrypt into a fresh folder, then swap it in for
`memory\` and `lore\` with the app's memory thread stopped. The old
folders are kept beside, renamed, never deleted by Horadric. The keeper
then embeds everything again in the background.

## The Settings section

"Backup", in the Settings window: where to (Off, Google Drive, a
folder), the Google account with Sign in or Sign out, the passphrase
(set, change), the last backup's time and size and any error, Back up
now, and Restore. Off by default.

## The parts

1. The backup itself, to a folder, and restore: format, crypto, the
   `Vault` trait, the folder vault, `horadric backup now|status|restore`.
   Done 2026-10-16, in `crates/horadric-ui/src/backup/`. Beyond the
   above: the manifest carries a value sealed under the key, so a wrong
   passphrase is caught even before the first blob; a file gone from the
   store stays in the backup, since an emptied store is the moment the
   backup must not follow it; the Sanctuary's layout
   (`lore\*.sanctuary.json`) is left out with the code index; and the
   command line's restore asks for Horadric to be quit, stopping only
   the memory thread is for the Settings button.
2. Google Drive: sign in, WinHTTP's other verbs, the Drive vault.
3. The Settings section and the schedule in the app.
