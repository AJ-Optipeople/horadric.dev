//! Serves this desktop to a viewer page on 127.0.0.1, for a machine that
//! watches it through an SSH tunnel: frames as JPEG, input posted back,
//! and the sessions waiting here. Off unless `HORADRIC_SCREEN_SHARE=1`.
//! What it parses and checks is in `horadric_core::screen_share`.
//!
//! It runs in the app because the app is in the interactive desktop
//! session: a service or an SSH login sees no screen and can send it no
//! keys.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use horadric_core::screen_share::{self as share, Button, Input, Mode, SessionRow, Size};
use horadric_core::Registry;
use windows::core::{Error, PCWSTR, PWSTR};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Gdi::{
    BitBlt, ChangeDisplaySettingsExW, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC,
    DeleteObject, EnumDisplayDevicesW, EnumDisplaySettingsW, GetDC, ReleaseDC, SelectObject,
    SetBrushOrgEx, SetStretchBltMode, StretchBlt, CAPTUREBLT, CDS_UPDATEREGISTRY, DEVMODEW,
    DISPLAY_DEVICEW, DISPLAY_DEVICE_PRIMARY_DEVICE, DISP_CHANGE_SUCCESSFUL, DM_BITSPERPEL,
    DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH, ENUM_CURRENT_SETTINGS,
    ENUM_DISPLAY_SETTINGS_MODE, HALFTONE, HBITMAP, HDC, HGDIOBJ, HPALETTE, SRCCOPY,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_WICPixelFormat24bppBGR,
    IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapEncoderNoCache, WICBitmapIgnoreAlpha,
    WICBitmapPaletteTypeCustom,
};
use windows::Win32::System::Com::StructuredStorage::{IPropertyBag2, PROPBAG2};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    STREAM_SEEK_END, STREAM_SEEK_SET,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
    KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::{
    DrawIconEx, GetCursorInfo, GetIconInfo, GetSystemMetrics, CURSORINFO, CURSOR_SHOWING,
    DI_NORMAL, HICON, ICONINFO, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};

const VIEWER: &str = include_str!("screen_share.html");

/// Starts serving when [`share::ENV`] says so, on threads of its own.
/// `registry` is the app's, read under its lock for `/sessions`.
pub fn start(registry: Arc<Mutex<Registry>>) {
    if !share::enabled(std::env::var(share::ENV).ok().as_deref()) {
        return;
    }
    let Some(token) = token() else {
        eprintln!("horadric: screen share: no token, not serving");
        return;
    };
    let port = share::port(horadric_hooks::dev());
    thread::spawn(move || {
        if let Err(e) = serve(port, &token, &registry) {
            eprintln!("horadric: screen share stopped: {e}");
        }
    });
}

/// The token in the state folder, written on first start and kept, so a
/// viewer opened once keeps working across restarts.
fn token() -> Option<String> {
    let path = horadric_hooks::state_dir()?.join(share::TOKEN_FILE);
    if let Some(t) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| share::read_token(&t))
    {
        return Some(t);
    }
    let mut bytes = [0u8; 16];
    crate::portal_crypto::random(&mut bytes);
    let token = share::token_hex(&bytes);
    write_token(&path, &token).ok()?;
    Some(token)
}

fn write_token(path: &Path, token: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, token)
}

fn serve(port: u16, token: &str, registry: &Arc<Mutex<Registry>>) -> io::Result<()> {
    let listener = bind(port)?;
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            // Out of handles or memory fails every accept alike: a pause
            // keeps that from taking a core while it lasts.
            Err(_) => {
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let token = token.to_string();
        let registry = Arc::clone(registry);
        thread::spawn(move || {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
            let _ = handle(&stream, &token, &registry);
        });
    }
    Ok(())
}

/// COM on this thread for as long as it lives, for WIC. Left again when
/// dropped, and only when joining succeeded, as COM counts them in pairs.
struct Com(bool);

impl Com {
    fn join() -> Com {
        // SAFETY: no pointers; the pair is kept by `Drop`.
        Com(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: matches the successful CoInitializeEx above, on the
            // same thread, since a `Com` is never sent anywhere.
            unsafe { CoUninitialize() };
        }
    }
}

/// Binds, retrying for a few seconds: after a reload the Horadric before
/// held the port until a moment ago.
fn bind(port: u16) -> io::Result<TcpListener> {
    let mut tries = 0;
    loop {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) => return Ok(l),
            Err(e) if tries >= 50 => return Err(e),
            Err(_) => {
                tries += 1;
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

const MAX_LINE: u64 = 8 * 1024;
const MAX_HEADERS: usize = 64;

fn handle(stream: &TcpStream, token: &str, registry: &Mutex<Registry>) -> io::Result<()> {
    // A borrow, never a `try_clone`: the clone is an inheritable handle a
    // session started meanwhile would keep open.
    let mut reader = BufReader::new(stream);
    let Some(request_line) = read_line(&mut reader)? else {
        return respond(stream, "431 Request Header Fields Too Large", "", b"");
    };
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();

    let mut content_length = 0usize;
    for count in 0.. {
        if count > MAX_HEADERS {
            return respond(stream, "431 Request Header Fields Too Large", "", b"");
        }
        let Some(line) = read_line(&mut reader)? else {
            return respond(stream, "431 Request Header Fields Too Large", "", b"");
        };
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }

    let (path, query) = share::split_target(&target);
    // The tunnel's far end is any local port, so the Host header says
    // nothing; the token is the guard on every path that shows or moves
    // anything. The viewer page itself holds no data and is served
    // without it, because it drops the token from its address, so a
    // reload of the tab comes back without one and finds it in the tab.
    let page = method == "GET" && path == "/";
    if !page && !share::token_ok(share::query_param(query, "t").as_deref(), token) {
        return respond(stream, "403 Forbidden", "text/plain", b"forbidden");
    }
    match (method.as_str(), path) {
        ("GET", "/") => respond(
            stream,
            "200 OK",
            "text/html; charset=utf-8",
            VIEWER.as_bytes(),
        ),
        ("GET", "/frame.jpg") => {
            let _com = Com::join();
            match frame(share::max_width(query)) {
                Ok(f) => respond_frame(stream, &f),
                Err(e) => respond(
                    stream,
                    "500 Internal Server Error",
                    "text/plain",
                    e.to_string().as_bytes(),
                ),
            }
        }
        ("GET", "/sessions") => respond(
            stream,
            "200 OK",
            "application/json",
            sessions(registry).as_bytes(),
        ),
        ("POST", "/input") => {
            let Some(body) = read_body(stream, &mut reader, content_length)? else {
                return Ok(());
            };
            match body.and_then(|b| share::parse_input(&b)) {
                Ok(input) => match send(&input) {
                    Ok(()) => respond(stream, "204 No Content", "", b""),
                    Err(why) => respond(stream, "409 Conflict", "text/plain", why.as_bytes()),
                },
                Err(why) => respond(stream, "400 Bad Request", "text/plain", why.as_bytes()),
            }
        }
        ("GET", "/display") => {
            let display = Display::primary();
            let current = display.current();
            let sizes = share::display_sizes(&display.modes(), current.bits);
            respond(
                stream,
                "200 OK",
                "application/json",
                share::display_json(current.size(), &sizes).as_bytes(),
            )
        }
        ("POST", "/display") => {
            let Some(body) = read_body(stream, &mut reader, content_length)? else {
                return Ok(());
            };
            match body.and_then(|b| share::parse_size(&b)) {
                Ok(size) => match Display::primary().resize(size) {
                    Ok(()) => respond(stream, "204 No Content", "", b""),
                    Err(Refused::NotOffered) => respond(
                        stream,
                        "400 Bad Request",
                        "text/plain",
                        b"not a resolution this display offers",
                    ),
                    Err(Refused::Windows(why)) => {
                        respond(stream, "409 Conflict", "text/plain", why.as_bytes())
                    }
                },
                Err(why) => respond(stream, "400 Bad Request", "text/plain", why.as_bytes()),
            }
        }
        _ => respond(stream, "404 Not Found", "text/plain", b"not found"),
    }
}

/// A posted body as text, or why it is not text. None when it was too big,
/// which is answered here already.
fn read_body(
    stream: &TcpStream,
    reader: &mut BufReader<&TcpStream>,
    length: usize,
) -> io::Result<Option<Result<String, String>>> {
    if length > share::MAX_BODY {
        respond(stream, "413 Payload Too Large", "", b"")?;
        drain(stream, reader);
        return Ok(None);
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    Ok(Some(String::from_utf8(body).map_err(|e| e.to_string())))
}

/// Reads and drops what is left of a body not read, a bounded amount for
/// a short while. Closing with bytes unread makes Windows reset the
/// connection, and the client then sees the reset, not the answer sent.
fn drain(stream: &TcpStream, reader: &mut impl Read) {
    const MOST: u64 = 1024 * 1024;
    let _ = stream.shutdown(Shutdown::Write);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let _ = io::copy(&mut reader.take(MOST), &mut io::sink());
}

fn read_line(reader: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    let read = reader.take(MAX_LINE).read_until(b'\n', &mut line)?;
    Ok((read < MAX_LINE as usize || line.ends_with(b"\n"))
        .then(|| String::from_utf8_lossy(&line).into_owned()))
}

fn respond(mut stream: &TcpStream, status: &str, kind: &str, body: &[u8]) -> io::Result<()> {
    let kind = if kind.is_empty() {
        String::new()
    } else {
        format!("Content-Type: {kind}\r\n")
    };
    let head = format!(
        "HTTP/1.1 {status}\r\n{kind}Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn respond_frame(mut stream: &TcpStream, f: &Frame) -> io::Result<()> {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\
         X-Screen-Left: {}\r\nX-Screen-Top: {}\r\nX-Screen-Width: {}\r\nX-Screen-Height: {}\r\n\
         Access-Control-Expose-Headers: X-Screen-Left, X-Screen-Top, X-Screen-Width, X-Screen-Height\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        f.jpeg.len(),
        f.screen.left,
        f.screen.top,
        f.screen.width,
        f.screen.height,
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(&f.jpeg)?;
    stream.flush()
}

/// The sessions as `/sessions` answers. The registry's lock is held only
/// to copy them: naming a project may ask git, which is slow.
fn sessions(registry: &Mutex<Registry>) -> String {
    let copied: Vec<_> = match registry.lock() {
        Ok(r) => r.all().cloned().collect(),
        Err(_) => Vec::new(),
    };
    let rows = copied
        .iter()
        .map(|s| SessionRow {
            id: s.id.clone(),
            name: s.label().to_string(),
            project: crate::project::project_name(&crate::project::project_key(s)),
            state: s.phase.label().to_string(),
            // While it waits, the last line is the question or what it
            // asks leave to do.
            waiting: (s.phase.is_waiting()).then(|| s.last_line.clone()),
        })
        .collect();
    share::sessions_json(rows)
}

/// The virtual screen: every monitor, in physical pixels, since the app is
/// per monitor DPI aware.
#[derive(Debug, Clone, Copy)]
struct Screen {
    left: i32,
    top: i32,
    width: i32,
    height: i32,
}

fn screen() -> Screen {
    unsafe {
        Screen {
            left: GetSystemMetrics(SM_XVIRTUALSCREEN),
            top: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    }
}

struct Frame {
    screen: Screen,
    jpeg: Vec<u8>,
}

/// GDI objects made for one frame, freed however the frame ends.
struct Gdi {
    screen: HDC,
    dcs: Vec<HDC>,
    bitmaps: Vec<HBITMAP>,
}

impl Drop for Gdi {
    fn drop(&mut self) {
        unsafe {
            for dc in &self.dcs {
                let _ = DeleteDC(*dc);
            }
            for b in &self.bitmaps {
                let _ = DeleteObject(HGDIOBJ(b.0));
            }
            ReleaseDC(None, self.screen);
        }
    }
}

impl Gdi {
    /// A memory DC holding a new `w` by `h` bitmap.
    unsafe fn canvas(&mut self, w: i32, h: i32) -> windows::core::Result<(HDC, HBITMAP)> {
        let dc = CreateCompatibleDC(Some(self.screen));
        if dc.is_invalid() {
            return Err(Error::from(E_FAIL));
        }
        self.dcs.push(dc);
        let bitmap = CreateCompatibleBitmap(self.screen, w, h);
        if bitmap.is_invalid() {
            return Err(Error::from(E_FAIL));
        }
        self.bitmaps.push(bitmap);
        SelectObject(dc, HGDIOBJ(bitmap.0));
        Ok((dc, bitmap))
    }
}

/// The whole virtual screen with the cursor drawn in, at most `max` wide,
/// as JPEG.
fn frame(max: Option<u32>) -> windows::core::Result<Frame> {
    let screen = screen();
    if screen.width <= 0 || screen.height <= 0 {
        return Err(Error::from(E_FAIL));
    }
    let (w, h) = share::scaled(screen.width as u32, screen.height as u32, max);
    let (w, h) = (w as i32, h as i32);
    unsafe {
        let mut gdi = Gdi {
            screen: GetDC(None),
            dcs: Vec::new(),
            bitmaps: Vec::new(),
        };
        if gdi.screen.is_invalid() {
            return Err(Error::from(E_FAIL));
        }
        let (full, full_bitmap) = gdi.canvas(screen.width, screen.height)?;
        // CAPTUREBLT takes layered windows too: menus, tooltips, toasts.
        BitBlt(
            full,
            0,
            0,
            screen.width,
            screen.height,
            Some(gdi.screen),
            screen.left,
            screen.top,
            SRCCOPY | CAPTUREBLT,
        )?;
        draw_cursor(full, screen);
        let bitmap = if (w, h) == (screen.width, screen.height) {
            full_bitmap
        } else {
            let (small, small_bitmap) = gdi.canvas(w, h)?;
            SetStretchBltMode(small, HALFTONE);
            let _ = SetBrushOrgEx(small, 0, 0, None);
            let _ = StretchBlt(
                small,
                0,
                0,
                w,
                h,
                Some(full),
                0,
                0,
                screen.width,
                screen.height,
                SRCCOPY,
            );
            small_bitmap
        };
        // WIC reads a bitmap that no DC has selected more surely.
        for dc in gdi.dcs.drain(..) {
            let _ = DeleteDC(dc);
        }
        let jpeg = encode_jpeg(bitmap)?;
        Ok(Frame { screen, jpeg })
    }
}

/// The cursor where it is, so the viewer sees what the mouse points at:
/// a screen copy leaves it out.
unsafe fn draw_cursor(dc: HDC, screen: Screen) {
    let mut info = CURSORINFO {
        cbSize: std::mem::size_of::<CURSORINFO>() as u32,
        ..Default::default()
    };
    if GetCursorInfo(&mut info).is_err()
        || info.flags.0 & CURSOR_SHOWING.0 == 0
        || info.hCursor.is_invalid()
    {
        return;
    }
    let icon = HICON(info.hCursor.0);
    let mut icon_info = ICONINFO::default();
    let (hot_x, hot_y) = if GetIconInfo(icon, &mut icon_info).is_ok() {
        // GetIconInfo hands over copies of the cursor's bitmaps.
        if !icon_info.hbmMask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(icon_info.hbmMask.0));
        }
        if !icon_info.hbmColor.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(icon_info.hbmColor.0));
        }
        (icon_info.xHotspot as i32, icon_info.yHotspot as i32)
    } else {
        (0, 0)
    };
    let _ = DrawIconEx(
        dc,
        info.ptScreenPos.x - screen.left - hot_x,
        info.ptScreenPos.y - screen.top - hot_y,
        icon,
        0,
        0,
        0,
        None,
        DI_NORMAL,
    );
}

/// JPEG through the Windows Imaging Component, which ships with Windows
/// and so costs no dependency.
unsafe fn encode_jpeg(bitmap: HBITMAP) -> windows::core::Result<Vec<u8>> {
    let factory: IWICImagingFactory =
        CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
    let source =
        factory.CreateBitmapFromHBITMAP(bitmap, HPALETTE::default(), WICBitmapIgnoreAlpha)?;
    let converter = factory.CreateFormatConverter()?;
    converter.Initialize(
        &source,
        &GUID_WICPixelFormat24bppBGR,
        WICBitmapDitherTypeNone,
        None,
        0.0,
        WICBitmapPaletteTypeCustom,
    )?;
    let stream = SHCreateMemStream(None).ok_or_else(|| Error::from(E_FAIL))?;
    let encoder = factory.CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null())?;
    encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
    let mut frame = None;
    let mut options: Option<IPropertyBag2> = None;
    encoder.CreateNewFrame(&mut frame, &mut options)?;
    let frame = frame.ok_or_else(|| Error::from(E_FAIL))?;
    if let Some(options) = &options {
        // Text stays readable at 0.7, and a frame is a third of what the
        // default 0.9 makes.
        let mut name: Vec<u16> = "ImageQuality\0".encode_utf16().collect();
        let bag = PROPBAG2 {
            pstrName: PWSTR(name.as_mut_ptr()),
            ..Default::default()
        };
        let _ = options.Write(1, &bag, &VARIANT::from(0.7f32));
    }
    frame.Initialize(options.as_ref())?;
    frame.WriteSource(&converter, std::ptr::null())?;
    frame.Commit()?;
    encoder.Commit()?;

    let mut size = 0u64;
    stream.Seek(0, STREAM_SEEK_END, Some(&mut size))?;
    stream.Seek(0, STREAM_SEEK_SET, None)?;
    let mut jpeg = vec![0u8; size as usize];
    let mut read = 0u32;
    stream
        .Read(jpeg.as_mut_ptr().cast(), jpeg.len() as u32, Some(&mut read))
        .ok()?;
    jpeg.truncate(read as usize);
    Ok(jpeg)
}

/// Plays one event into this desktop, or says why Windows took less of it
/// than was given.
fn send(input: &Input) -> Result<(), String> {
    let screen = screen();
    let at = |x: i32, y: i32| {
        (
            share::absolute(x, screen.width),
            share::absolute(y, screen.height),
        )
    };
    let inputs = match input {
        Input::Move { x, y } => vec![mouse(at(*x, *y), MOUSE_EVENT_FLAGS(0), 0)],
        Input::Down { x, y, button } => {
            let flag = match button {
                Button::Left => MOUSEEVENTF_LEFTDOWN,
                Button::Right => MOUSEEVENTF_RIGHTDOWN,
                Button::Middle => MOUSEEVENTF_MIDDLEDOWN,
            };
            vec![mouse(at(*x, *y), flag, 0)]
        }
        Input::Up { x, y, button } => {
            let flag = match button {
                Button::Left => MOUSEEVENTF_LEFTUP,
                Button::Right => MOUSEEVENTF_RIGHTUP,
                Button::Middle => MOUSEEVENTF_MIDDLEUP,
            };
            vec![mouse(at(*x, *y), flag, 0)]
        }
        // The wheel turns what is under the pointer, so it goes there first.
        Input::Wheel { x, y, delta } => vec![mouse(at(*x, *y), MOUSEEVENTF_WHEEL, *delta)],
        Input::Key { vk, down } => {
            let mut flags = if *down {
                KEYBD_EVENT_FLAGS(0)
            } else {
                KEYEVENTF_KEYUP
            };
            if share::extended_key(*vk) {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            // Some programs read the scan code, not the virtual key.
            let scan = unsafe { MapVirtualKeyW(u32::from(*vk), MAPVK_VK_TO_VSC) } as u16;
            vec![key(VIRTUAL_KEY(*vk), scan, flags)]
        }
        Input::Text { text } => text
            .encode_utf16()
            .flat_map(|unit| {
                [
                    key(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE),
                    key(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
                ]
            })
            .collect(),
    };
    // A long text in one SendInput outruns the program reading its queue:
    // Notepad dropped the rest of the text, or repeated its last character.
    for (i, piece) in inputs.chunks(40).enumerate() {
        if i > 0 {
            thread::sleep(Duration::from_millis(15));
        }
        // SAFETY: the slice and the size describe the same INPUTs.
        let sent = unsafe { SendInput(piece, std::mem::size_of::<INPUT>() as i32) };
        // SendInput says nothing more than a short count: these two are
        // what makes it refuse in practice.
        if (sent as usize) < piece.len() {
            return Err(
                "input blocked: the window under it runs elevated, or the desktop is locked".into(),
            );
        }
    }
    Ok(())
}

fn mouse((dx, dy): (i32, i32), flags: MOUSE_EVENT_FLAGS, wheel: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: wheel as u32,
                dwFlags: flags | MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// The primary display, by its device name, so the resolution read and
/// the one set are of the same monitor.
struct Display {
    /// The device name, NUL ended, or empty when no device said it was
    /// primary: Windows then takes its default display, which is it.
    name: Vec<u16>,
}

/// Why a resolution was not set.
enum Refused {
    NotOffered,
    Windows(String),
}

impl Display {
    fn primary() -> Display {
        let mut device = DISPLAY_DEVICEW {
            cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        for i in 0.. {
            // SAFETY: `device` is a DISPLAY_DEVICEW with `cb` set, as asked.
            if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), i, &mut device, 0) }.as_bool() {
                break;
            }
            if device.StateFlags.0 & DISPLAY_DEVICE_PRIMARY_DEVICE.0 != 0 {
                let end = (device.DeviceName.iter().position(|c| *c == 0))
                    .unwrap_or(device.DeviceName.len());
                let mut name = device.DeviceName[..end].to_vec();
                name.push(0);
                return Display { name };
            }
        }
        Display { name: Vec::new() }
    }

    fn device(&self) -> PCWSTR {
        if self.name.is_empty() {
            PCWSTR::null()
        } else {
            PCWSTR(self.name.as_ptr())
        }
    }

    fn settings(&self, which: ENUM_DISPLAY_SETTINGS_MODE) -> Option<DEVMODEW> {
        let mut mode = DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        // SAFETY: the name outlives the call and ends in NUL, and `mode`
        // is a DEVMODEW with `dmSize` set, as asked.
        unsafe { EnumDisplaySettingsW(self.device(), which, &mut mode) }
            .as_bool()
            .then_some(mode)
    }

    /// The mode in use, all zero when Windows will not say.
    fn current(&self) -> Mode {
        self.settings(ENUM_CURRENT_SETTINGS)
            .as_ref()
            .map(mode_of)
            .unwrap_or(Mode {
                width: 0,
                height: 0,
                bits: 0,
                refresh: 0,
            })
    }

    /// Every mode the driver lists, refresh rates and all.
    fn all(&self) -> Vec<DEVMODEW> {
        (0..)
            .map_while(|i| self.settings(ENUM_DISPLAY_SETTINGS_MODE(i)))
            .collect()
    }

    fn modes(&self) -> Vec<Mode> {
        self.all().iter().map(mode_of).collect()
    }

    /// Switches to `size` for good, written to the registry. It takes the
    /// driver's own mode for the size rather than the current one with a
    /// new size edited in, which the driver may not take.
    fn resize(&self, size: Size) -> Result<(), Refused> {
        let current = self.current();
        let all = self.all();
        let modes: Vec<Mode> = all.iter().map(mode_of).collect();
        if !share::display_sizes(&modes, current.bits).contains(&size) {
            return Err(Refused::NotOffered);
        }
        let picked = share::pick_mode(&modes, size, current.bits, current.refresh)
            .and_then(|m| modes.iter().position(|o| *o == m))
            .ok_or(Refused::NotOffered)?;
        let mut mode = all[picked];
        mode.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT;
        if mode.dmBitsPerPel != 0 {
            mode.dmFields |= DM_BITSPERPEL;
        }
        // 0 and 1 stand for the hardware's default rate, not a rate.
        if mode.dmDisplayFrequency > 1 {
            mode.dmFields |= DM_DISPLAYFREQUENCY;
        }
        // SAFETY: `mode` came from EnumDisplaySettingsW with its size set,
        // and the name outlives the call.
        let code = unsafe {
            ChangeDisplaySettingsExW(self.device(), Some(&mode), None, CDS_UPDATEREGISTRY, None)
        };
        if code == DISP_CHANGE_SUCCESSFUL {
            Ok(())
        } else {
            Err(Refused::Windows(format!(
                "Windows refused the resolution: {}",
                share::disp_change_name(code.0)
            )))
        }
    }
}

fn mode_of(m: &DEVMODEW) -> Mode {
    Mode {
        width: m.dmPelsWidth,
        height: m.dmPelsHeight,
        bits: m.dmBitsPerPel,
        refresh: m.dmDisplayFrequency,
    }
}

fn key(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}
