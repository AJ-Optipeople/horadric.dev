//! Secrets in Windows Credential Manager, as generic credentials: a
//! name, a user name shown beside it, and a few hundred bytes that only
//! this Windows account can read. The Google refresh token lives here.

use windows::core::{HSTRING, PWSTR};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_FLAGS,
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
};

/// The user name and secret stored under `target`, if any.
pub fn read(target: &str) -> Option<(String, Vec<u8>)> {
    unsafe {
        let mut found: *mut CREDENTIALW = std::ptr::null_mut();
        CredReadW(&HSTRING::from(target), CRED_TYPE_GENERIC, None, &mut found).ok()?;
        let c = &*found;
        let user = if c.UserName.is_null() {
            String::new()
        } else {
            c.UserName.to_string().unwrap_or_default()
        };
        let secret = if c.CredentialBlob.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec()
        };
        CredFree(found as *const _);
        Some((user, secret))
    }
}

/// Writes or replaces `target`. Kept on this machine only, never roamed
/// with a domain profile.
pub fn write(target: &str, user: &str, secret: &[u8]) -> Result<(), String> {
    let mut target_w: Vec<u16> = target.encode_utf16().chain([0]).collect();
    let mut user_w: Vec<u16> = user.encode_utf16().chain([0]).collect();
    let mut blob = secret.to_vec();
    let credential = CREDENTIALW {
        Flags: CRED_FLAGS(0),
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target_w.as_mut_ptr()),
        Comment: PWSTR::null(),
        LastWritten: FILETIME::default(),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        AttributeCount: 0,
        Attributes: std::ptr::null_mut(),
        TargetAlias: PWSTR::null(),
        UserName: PWSTR(user_w.as_mut_ptr()),
    };
    unsafe { CredWriteW(&credential, 0) }
        .map_err(|e| format!("cannot write to Credential Manager: {e}"))
}

/// Forgets `target`. Already gone is fine.
pub fn delete(target: &str) {
    unsafe {
        let _ = CredDeleteW(&HSTRING::from(target), CRED_TYPE_GENERIC, None);
    }
}
