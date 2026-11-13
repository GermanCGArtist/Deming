//! Native Windows HWND screenshot capture by PID, DWM window cloaking, and `<instance>/screenshots/*.png` indexing.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use deming_core::{DemingError, Result};
use image::{imageops::FilterType, RgbaImage};

/// Indexes all `.png` screenshots inside `<instance_dir>/screenshots/`, sorted lexicographically.
pub fn index_screenshots(instance_dir: &Path) -> Result<Vec<PathBuf>> {
    let screenshots_dir = instance_dir.join("screenshots");
    if !screenshots_dir.exists() {
        return Ok(Vec::new());
    }

    let mut paths = Vec::new();
    for entry in fs::read_dir(&screenshots_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            let is_png = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("png"))
                .unwrap_or(false);
            if is_png {
                paths.push(path);
            }
        }
    }
    paths.sort();
    Ok(paths)
}

/// Captures the main visible window of `pid` into `<instance_dir>/screenshots/<timestamp>[_<label>].png`.
pub fn capture_instance_screenshot(
    pid: u32,
    instance_dir: &Path,
    label: Option<&str>,
) -> Result<PathBuf> {
    let screenshots_dir = instance_dir.join("screenshots");
    fs::create_dir_all(&screenshots_dir)?;

    let ts = Utc::now().format("%Y%m%d_%H%M%S_%3f");
    let filename = match label {
        Some(lbl) if !lbl.trim().is_empty() => {
            let safe: String = lbl
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            format!("{ts}_{safe}.png")
        }
        _ => format!("{ts}.png"),
    };

    let output_path = screenshots_dir.join(filename);
    capture_pid_to_png(pid, &output_path)?;
    Ok(output_path)
}

/// Returns a list of visible (including DWM-cloaked) non-zero-sized top-level window handles
/// (`isize`) owned by `pid`, ordered largest area first.
#[cfg(windows)]
pub fn find_windows_for_pid(pid: u32) -> Vec<isize> {
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible,
    };

    struct EnumState {
        target_pid: u32,
        matches: Vec<(isize, i64)>,
    }

    unsafe extern "system" fn enum_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let state = &mut *(lparam as *mut EnumState);
        let mut window_pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &raw mut window_pid);
        // IsWindowVisible checks WS_VISIBLE and remains true for DWM-cloaked windows.
        if window_pid == state.target_pid && IsWindowVisible(hwnd) != 0 {
            let mut rect: RECT = std::mem::zeroed();
            if GetWindowRect(hwnd, &raw mut rect) != 0 {
                let width = (rect.right - rect.left) as i64;
                let height = (rect.bottom - rect.top) as i64;
                if width > 0 && height > 0 {
                    state.matches.push((hwnd as isize, width * height));
                }
            }
        }
        1 // TRUE: continue enumeration
    }

    let mut state = EnumState {
        target_pid: pid,
        matches: Vec::new(),
    };

    unsafe {
        EnumWindows(Some(enum_callback), (&raw mut state) as LPARAM);
    }

    // Prefer largest visible window first (main game window over tiny helper windows).
    state.matches.sort_by(|a, b| b.1.cmp(&a.1));
    state.matches.into_iter().map(|(h, _)| h).collect()
}

/// Fallback stub on non-Windows platforms.
#[cfg(not(windows))]
pub fn find_windows_for_pid(_pid: u32) -> Vec<isize> {
    Vec::new()
}

/// Cloaks (`cloak = true`) or uncloaks (`cloak = false`) all top-level windows belonging to `pid`
/// using `DWMWA_CLOAK` and toggles `WS_EX_TOOLWINDOW` / `WS_EX_APPWINDOW` so cloaked windows are
/// hidden from the desktop, taskbar, and Alt-Tab while still renderable via `PrintWindow`.
///
/// Returns the number of matching top-level windows updated.
#[cfg(windows)]
pub fn cloak_and_hide_from_taskbar(pid: u32, cloak: bool) -> Result<usize> {
    use std::mem::size_of;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, SWP_FRAMECHANGED,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    };

    let windows = find_windows_for_pid(pid);
    if windows.is_empty() {
        return Ok(0);
    }

    let mut updated = 0usize;
    for hwnd_raw in windows {
        let hwnd = hwnd_raw as HWND;
        let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as usize;

        if cloak {
            let new_style = (ex_style | WS_EX_TOOLWINDOW as usize) & !(WS_EX_APPWINDOW as usize);
            unsafe {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_style as isize);
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                );
            }
            let cloak_val: u32 = 1;
            let hr = unsafe {
                DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_CLOAK as u32,
                    (&raw const cloak_val).cast(),
                    size_of::<u32>() as u32,
                )
            };
            if hr >= 0 {
                updated += 1;
            }
        } else {
            let cloak_val: u32 = 0;
            let hr = unsafe {
                DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_CLOAK as u32,
                    (&raw const cloak_val).cast(),
                    size_of::<u32>() as u32,
                )
            };
            let new_style = (ex_style & !(WS_EX_TOOLWINDOW as usize)) | (WS_EX_APPWINDOW as usize);
            unsafe {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_style as isize);
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                );
            }
            if hr >= 0 {
                updated += 1;
            }
        }
    }

    Ok(updated)
}

/// Non-Windows stub returning `Ok(0)`.
#[cfg(not(windows))]
pub fn cloak_and_hide_from_taskbar(_pid: u32, _cloak: bool) -> Result<usize> {
    Ok(0)
}

/// Captures the largest visible (or cloaked) window belonging to `pid` and returns
/// `(width, height, rgba_bytes)` downscaled proportionally so `width <= max_width` (when `max_width > 0`).
///
/// Returns `Ok(None)` when `pid` currently has no visible top-level window.
#[cfg(windows)]
pub fn capture_pid_rgba(pid: u32, max_width: u32) -> Result<Option<(u32, u32, Vec<u8>)>> {
    use windows_sys::Win32::Foundation::HWND;

    let windows = find_windows_for_pid(pid);
    let Some(hwnd_raw) = windows.into_iter().next() else {
        return Ok(None);
    };

    let (width, height, rgba) = capture_hwnd_rgba(hwnd_raw as HWND, pid)?;
    let scaled = downscale_rgba(width, height, rgba, max_width)?;
    Ok(Some(scaled))
}

/// Non-Windows stub returning `Ok(None)`.
#[cfg(not(windows))]
pub fn capture_pid_rgba(_pid: u32, _max_width: u32) -> Result<Option<(u32, u32, Vec<u8>)>> {
    Ok(None)
}

/// Captures the largest visible window belonging to `pid` and saves it as a PNG at `output_path`.
#[cfg(windows)]
pub fn capture_pid_to_png(pid: u32, output_path: &Path) -> Result<PathBuf> {
    use image::ImageFormat;
    use windows_sys::Win32::Foundation::HWND;

    let windows = find_windows_for_pid(pid);
    let hwnd_raw = windows.into_iter().next().ok_or_else(|| {
        DemingError::Process(format!("no visible window found for PID {pid}"))
    })?;

    let (width, height, rgba_bytes) = capture_hwnd_rgba(hwnd_raw as HWND, pid)?;

    let img = RgbaImage::from_raw(width, height, rgba_bytes).ok_or_else(|| {
        DemingError::Other("failed to construct RGBA image buffer from DIB".to_string())
    })?;

    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    img.save_with_format(output_path, ImageFormat::Png)
        .map_err(|e| DemingError::Other(format!("failed to save PNG screenshot: {e}")))?;

    Ok(output_path.to_path_buf())
}

/// Non-Windows stub returning an unsupported error.
#[cfg(not(windows))]
pub fn capture_pid_to_png(_pid: u32, _output_path: &Path) -> Result<PathBuf> {
    Err(DemingError::Other(
        "native HWND screenshot capture is only supported on Windows".to_string(),
    ))
}

#[cfg(windows)]
fn capture_hwnd_rgba(
    hwnd: windows_sys::Win32::Foundation::HWND,
    pid: u32,
) -> Result<(u32, u32, Vec<u8>)> {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        HDC, SRCCOPY,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect;

    // PrintWindow is exported by user32.dll on Windows; declaring it directly keeps
    // windows-sys features minimal.
    #[link(name = "user32")]
    extern "system" {
        fn PrintWindow(hwnd: HWND, hdcblt: HDC, nflags: u32) -> i32;
    }
    const PW_RENDERFULLCONTENT: u32 = 0x0000_0002;

    let mut rect: RECT = unsafe { zeroed() };
    if unsafe { GetWindowRect(hwnd, &raw mut rect) } == 0 {
        return Err(DemingError::Process(format!(
            "GetWindowRect failed for PID {pid}: {}",
            std::io::Error::last_os_error()
        )));
    }

    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width <= 0 || height <= 0 {
        return Err(DemingError::Process(format!(
            "window for PID {pid} has invalid dimensions ({width}x{height})"
        )));
    }

    let wnd_dc = unsafe { GetDC(hwnd) };
    if wnd_dc.is_null() {
        return Err(DemingError::Process(format!(
            "GetDC failed for PID {pid}: {}",
            std::io::Error::last_os_error()
        )));
    }

    let mem_dc = unsafe { CreateCompatibleDC(wnd_dc) };
    if mem_dc.is_null() {
        unsafe {
            ReleaseDC(hwnd, wnd_dc);
        }
        return Err(DemingError::Process(
            "CreateCompatibleDC failed".to_string(),
        ));
    }

    let bitmap = unsafe { CreateCompatibleBitmap(wnd_dc, width, height) };
    if bitmap.is_null() {
        unsafe {
            DeleteDC(mem_dc);
            ReleaseDC(hwnd, wnd_dc);
        }
        return Err(DemingError::Process(
            "CreateCompatibleBitmap failed".to_string(),
        ));
    }

    let old_obj = unsafe { SelectObject(mem_dc, bitmap) };

    // Try PrintWindow with PW_RENDERFULLCONTENT first (works on DWM-cloaked windows);
    // fall back to standard PrintWindow / BitBlt if needed.
    let mut captured = unsafe { PrintWindow(hwnd, mem_dc, PW_RENDERFULLCONTENT) } != 0;
    if !captured {
        captured = unsafe { PrintWindow(hwnd, mem_dc, 0) } != 0;
    }
    if !captured {
        captured = unsafe { BitBlt(mem_dc, 0, 0, width, height, wnd_dc, 0, 0, SRCCOPY) } != 0;
    }

    let mut rgba_bytes = vec![0u8; (width as usize) * (height as usize) * 4];
    let mut dib_ok = false;
    if captured {
        let mut bmi: BITMAPINFO = unsafe { zeroed() };
        bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = width;
        bmi.bmiHeader.biHeight = -height; // top-down DIB
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;

        let scanlines = unsafe {
            GetDIBits(
                mem_dc,
                bitmap,
                0,
                height as u32,
                rgba_bytes.as_mut_ptr().cast(),
                &raw mut bmi,
                DIB_RGB_COLORS,
            )
        };
        dib_ok = scanlines != 0;
    }

    // Cleanup all GDI resources before converting/returning.
    unsafe {
        SelectObject(mem_dc, old_obj);
        DeleteObject(bitmap);
        DeleteDC(mem_dc);
        ReleaseDC(hwnd, wnd_dc);
    }

    if !captured || !dib_ok {
        return Err(DemingError::Process(format!(
            "failed to capture window contents for PID {pid}: {}",
            std::io::Error::last_os_error()
        )));
    }

    // Convert BGRA -> RGBA and normalize opaque alpha for captured desktop windows.
    for px in rgba_bytes.chunks_exact_mut(4) {
        px.swap(0, 2);
        px[3] = 255;
    }

    Ok((width as u32, height as u32, rgba_bytes))
}

/// Proportionally downscales an RGBA buffer so its width does not exceed `max_width`
/// (unless `max_width == 0`, which disables scaling).
pub(crate) fn downscale_rgba(
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    max_width: u32,
) -> Result<(u32, u32, Vec<u8>)> {
    if max_width == 0 || width <= max_width || width == 0 || height == 0 {
        return Ok((width, height, rgba));
    }

    let new_width = max_width;
    let new_height = ((height as u64 * max_width as u64) / width as u64).max(1) as u32;
    let img = RgbaImage::from_raw(width, height, rgba).ok_or_else(|| {
        DemingError::Other("invalid RGBA buffer length for downscaling".to_string())
    })?;
    let resized = image::imageops::resize(&img, new_width, new_height, FilterType::Triangle);
    Ok((new_width, new_height, resized.into_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downscale_rgba_scales_proportionally_and_skips_when_within_limit() {
        let src = vec![255u8; 800 * 600 * 4];

        // Within limit -> unchanged dimensions and buffer.
        let (w, h, buf) = downscale_rgba(800, 600, src.clone(), 800).unwrap();
        assert_eq!((w, h), (800, 600));
        assert_eq!(buf.len(), 800 * 600 * 4);

        // Zero max_width -> scaling disabled.
        let (w, h, buf) = downscale_rgba(800, 600, src.clone(), 0).unwrap();
        assert_eq!((w, h), (800, 600));
        assert_eq!(buf.len(), 800 * 600 * 4);

        // Downscale 800x600 -> max_width 320 => 320x240.
        let (w, h, buf) = downscale_rgba(800, 600, src, 320).unwrap();
        assert_eq!((w, h), (320, 240));
        assert_eq!(buf.len(), 320 * 240 * 4);
    }

    #[cfg(windows)]
    #[test]
    fn cloak_and_capture_return_gracefully_for_nonexistent_pid() {
        // PID u32::MAX - 1 will not match any running window.
        let nonexistent_pid = u32::MAX - 1;
        assert_eq!(cloak_and_hide_from_taskbar(nonexistent_pid, true).unwrap(), 0);
        assert_eq!(cloak_and_hide_from_taskbar(nonexistent_pid, false).unwrap(), 0);
        assert!(capture_pid_rgba(nonexistent_pid, 640).unwrap().is_none());
    }
}
