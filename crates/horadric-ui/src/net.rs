//! HTTPS through WinHTTP, which ships with Windows, follows GitHub's
//! redirects to its storage host and uses the system's proxy settings.
//! The updater GETs; the backup to Google Drive also POSTs, PATCHes, PUTs
//! and DELETEs. It is blocking, so its callers run on a thread.

use std::ffi::c_void;

use horadric_core::release::split_url;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption,
    WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_OPEN_REQUEST_FLAGS, WINHTTP_OPTION_REDIRECT_POLICY,
    WINHTTP_OPTION_REDIRECT_POLICY_NEVER, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_LOCATION,
    WINHTTP_QUERY_STATUS_CODE,
};

/// Closes a WinHTTP handle when it goes, so every early return is clean.
struct Handle(*mut c_void);

impl Handle {
    fn new(raw: *mut c_void, what: &str) -> Result<Handle, String> {
        if raw.is_null() {
            Err(format!("{what}: {}", windows::core::Error::from_thread()))
        } else {
            Ok(Handle(raw))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

/// The body of `url`, which must answer 200 and send at most `limit`
/// bytes.
pub fn get(url: &str, limit: usize) -> Result<Vec<u8>, String> {
    let answer = request("GET", url, &[], &[], limit)?;
    if answer.status != 200 {
        return Err(format!("{url} answered {}", answer.status));
    }
    Ok(answer.body)
}

/// What a server answered, whatever its status.
pub struct Answer {
    pub status: u32,
    /// The `Location` header, which a resumable upload's address comes in.
    pub location: Option<String>,
    pub body: Vec<u8>,
}

/// Sends `method` to `url` with `headers` and `body`, and reads at most
/// `limit` bytes back. Only a GET follows redirects: Drive answers an
/// unfinished upload with 308, which is not a redirect there. A slow
/// server gives up after a minute rather than hang the thread for good.
pub fn request(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    limit: usize,
) -> Result<Answer, String> {
    let parts = split_url(url).ok_or_else(|| format!("`{url}` is not an http or https URL"))?;
    let agent = format!("Horadric/{}", env!("CARGO_PKG_VERSION"));
    let headers: String = headers
        .iter()
        .map(|(k, v)| format!("{k}: {v}\r\n"))
        .collect();
    let headers: Vec<u16> = headers.encode_utf16().collect();
    let length = u32::try_from(body.len()).map_err(|_| "the body is over 4 GB".to_string())?;
    unsafe {
        let session = Handle::new(
            WinHttpOpen(
                &HSTRING::from(agent),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ),
            "WinHttpOpen",
        )?;
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 60_000, 60_000)
            .map_err(|e| format!("WinHttpSetTimeouts: {e}"))?;
        let connection = Handle::new(
            WinHttpConnect(
                session.0,
                &HSTRING::from(parts.host.as_str()),
                parts.port,
                0,
            ),
            "cannot connect",
        )?;
        let flags = if parts.secure {
            WINHTTP_FLAG_SECURE
        } else {
            WINHTTP_OPEN_REQUEST_FLAGS(0)
        };
        let request = Handle::new(
            WinHttpOpenRequest(
                connection.0,
                &HSTRING::from(method),
                &HSTRING::from(parts.path.as_str()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                flags,
            ),
            "WinHttpOpenRequest",
        )?;
        if method != "GET" {
            let never = WINHTTP_OPTION_REDIRECT_POLICY_NEVER.to_ne_bytes();
            WinHttpSetOption(
                Some(request.0),
                WINHTTP_OPTION_REDIRECT_POLICY,
                Some(&never),
            )
            .map_err(|e| format!("WinHttpSetOption: {e}"))?;
        }
        WinHttpSendRequest(
            request.0,
            (!headers.is_empty()).then_some(headers.as_slice()),
            (!body.is_empty()).then_some(body.as_ptr() as *const c_void),
            length,
            length,
            0,
        )
        .map_err(|e| format!("cannot reach {}: {e}", parts.host))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut())
            .map_err(|e| format!("no answer from {}: {e}", parts.host))?;

        let mut status = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut size,
            std::ptr::null_mut(),
        )
        .map_err(|e| format!("no status: {e}"))?;

        let mut location = vec![0u16; 4096];
        let mut size = (location.len() * 2) as u32;
        let location = WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_LOCATION,
            PCWSTR::null(),
            Some(location.as_mut_ptr() as *mut c_void),
            &mut size,
            std::ptr::null_mut(),
        )
        .ok()
        .map(|()| String::from_utf16_lossy(&location[..size as usize / 2]));

        let mut body = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr() as *mut c_void,
                chunk.len() as u32,
                &mut read,
            )
            .map_err(|e| format!("the download broke off: {e}"))?;
            if read == 0 {
                return Ok(Answer {
                    status,
                    location,
                    body,
                });
            }
            if body.len() + read as usize > limit {
                return Err(format!("{url} is larger than {limit} bytes"));
            }
            body.extend_from_slice(&chunk[..read as usize]);
        }
    }
}
