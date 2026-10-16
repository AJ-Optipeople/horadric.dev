//! Secrets in Windows Credential Manager, so backups run unattended: the
//! passphrase here, and Google's refresh token beside it. Each name
//! carries the state folder's, so a dev instance keeps its own.

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_FLAGS,
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
};

/// The passphrase's name.
pub const PASSPHRASE: &str = "backup passphrase";

/// What a secret is stored under: `Horadric-dev backup passphrase`. No
/// colon, which `cmdkey /delete` cannot take.
pub fn target(name: &str) -> String {
    format!("{} {name}", horadric_hooks::state_name())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// The secret stored under `name`, `None` when there is none.
pub fn read(name: &str) -> Option<String> {
    let target = wide(&target(name));
    let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
    unsafe {
        CredReadW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None, &mut cred).ok()?;
        let c = &*cred;
        let blob = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
        let text = String::from_utf8(blob.to_vec()).ok();
        CredFree(cred as *const _);
        text
    }
}

/// Stores `secret` under `name`, replacing what was there. Kept for this
/// user on this machine only, never roamed.
pub fn write(name: &str, secret: &str) -> Result<(), String> {
    let mut target = wide(&target(name));
    let mut user = wide("Horadric");
    let mut blob = secret.as_bytes().to_vec();
    let cred = CREDENTIALW {
        Flags: CRED_FLAGS(0),
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target.as_mut_ptr()),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: PWSTR(user.as_mut_ptr()),
        LastWritten: FILETIME::default(),
        ..Default::default()
    };
    unsafe { CredWriteW(&cred, 0) }.map_err(|e| format!("could not keep the secret: {e}"))
}

/// Forgets the secret under `name`; forgetting one that is not there is
/// not an error.
pub fn delete(name: &str) {
    let target = wide(&target(name));
    unsafe {
        let _ = CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_reads_and_deletes() {
        let name = format!("test secret {}", std::process::id());
        assert_eq!(read(&name), None);
        write(&name, "correct horse").unwrap();
        write(&name, "battery staple").unwrap();
        assert_eq!(read(&name).as_deref(), Some("battery staple"));
        delete(&name);
        assert_eq!(read(&name), None);
        delete(&name);
    }
}
