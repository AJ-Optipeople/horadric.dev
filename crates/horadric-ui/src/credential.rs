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
        .map_err(|e| write_failure(e.code().0 as u32, &e.message()))
}

/// Windows' cap on a generic credential's secret, `CRED_MAX_CREDENTIAL_BLOB_SIZE`.
const MAX_SECRET: usize = 2560;

/// Why a write failed, in words that say what to do. Windows reports a
/// full Credential Manager as out of memory, which reads like Horadric's
/// fault, and a secret over the cap as bad RPC data, which reads like
/// nothing at all.
fn write_failure(hresult: u32, windows_says: &str) -> String {
    match hresult {
        // ERROR_NOT_ENOUGH_MEMORY
        0x8007_0008 => "Windows Credential Manager is full, so it refuses every new entry. \
             This Windows account has too many saved sign ins, most often Xbox tokens \
             whose names start with XblGrts. Remove old entries in Control Panel, \
             Credential Manager, then try again."
            .into(),
        // RPC_X_BAD_STUB_DATA
        0x8007_06F7 => format!(
            "the secret is too large for Windows Credential Manager, which holds {MAX_SECRET} bytes at most"
        ),
        _ => format!("cannot write to Windows Credential Manager: {windows_says} (0x{hresult:08X})"),
    }
}

/// Forgets `target`. Already gone is fine.
pub fn delete(target: &str) {
    unsafe {
        let _ = CredDeleteW(&HSTRING::from(target), CRED_TYPE_GENERIC, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_credential_manager_is_named_as_full() {
        let why = write_failure(0x8007_0008, "Not enough memory resources");
        assert!(why.starts_with("Windows Credential Manager is full"));
        assert!(!why.contains("memory"));
    }

    #[test]
    fn a_secret_over_the_cap_says_the_cap() {
        assert!(write_failure(0x8007_06F7, "").contains("2560 bytes"));
    }

    #[test]
    fn anything_else_keeps_what_windows_said() {
        assert_eq!(
            write_failure(0x8007_0005, "Access is denied."),
            "cannot write to Windows Credential Manager: Access is denied. (0x80070005)"
        );
    }
}
