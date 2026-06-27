//! Headroom DLL adapter — optional dynamic loading of headroom-ffi.
//!
//! Uses platform-native dynamic loading (LoadLibrary/GetProcAddress on Windows,
//! dlopen/dlsym on Unix) — no `libloading` crate needed.

use std::ffi::{c_char, CStr};
use std::path::{Path, PathBuf};

use crate::compress::CompressionResult;

type CompressFn = unsafe extern "C" fn(*const c_char, u8) -> *mut c_char;
type RetrieveFn = unsafe extern "C" fn(*const c_char) -> *mut c_char;
type FreeFn = unsafe extern "C" fn(*mut c_char);
type CcrStatsFn = unsafe extern "C" fn() -> *mut c_char;

pub struct HeadroomDll {
    compress: CompressFn,
    retrieve: RetrieveFn,
    free: FreeFn,
    ccr_stats: CcrStatsFn,
}

unsafe impl Send for HeadroomDll {}
unsafe impl Sync for HeadroomDll {}

impl HeadroomDll {
    pub fn load() -> Option<Self> {
        let path = find_dll()?;
        tracing::info!("Loading headroom DLL: {}", path.display());
        let dll = load_platform_dll(&path)?;
        tracing::info!("Headroom DLL loaded successfully");
        Some(dll)
    }

    pub fn compress(&self, content: &str, content_type: u8) -> Option<CompressionResult> {
        let c_str = std::ffi::CString::new(content).ok()?;
        let result_ptr = unsafe { (self.compress)(c_str.as_ptr(), content_type) };
        if result_ptr.is_null() {
            return None;
        }
        let result_str = unsafe { CStr::from_ptr(result_ptr).to_string_lossy().to_string() };
        unsafe { (self.free)(result_ptr) };

        let parsed: serde_json::Value = serde_json::from_str(&result_str).ok()?;
        match parsed.get("status")?.as_str()? {
            "compressed" => Some(CompressionResult::Compressed {
                replacement: parsed["replacement"].as_str()?.to_string(),
                ccr_hash: parsed["ccr_hash"].as_str()?.to_string(),
                original_bytes: parsed["original_bytes"].as_u64()? as usize,
                compressed_bytes: parsed["compressed_bytes"].as_u64()? as usize,
            }),
            "unchanged" => Some(CompressionResult::Unchanged),
            _ => None,
        }
    }

    pub fn retrieve(&self, hash: &str) -> Option<String> {
        let c_str = std::ffi::CString::new(hash).ok()?;
        let result_ptr = unsafe { (self.retrieve)(c_str.as_ptr()) };
        if result_ptr.is_null() {
            return None;
        }
        let content = unsafe { CStr::from_ptr(result_ptr).to_string_lossy().to_string() };
        unsafe { (self.free)(result_ptr) };
        Some(content)
    }

    /// Query the DLL's CCR store statistics.
    /// Returns parsed JSON on success, `None` on failure.
    pub fn ccr_stats(&self) -> Option<serde_json::Value> {
        let result_ptr = unsafe { (self.ccr_stats)() };
        if result_ptr.is_null() {
            return None;
        }
        let json_str = unsafe { CStr::from_ptr(result_ptr).to_string_lossy().to_string() };
        unsafe { (self.free)(result_ptr) };
        serde_json::from_str(&json_str).ok()
    }
}

// ── Platform DLL loading ──────────────────────────────────────────

#[cfg(windows)]
fn load_platform_dll(path: &Path) -> Option<HeadroomDll> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let handle = unsafe { LoadLibraryW(wide.as_ptr()) };
    if handle.is_null() {
        tracing::warn!("LoadLibraryW failed for {}", path.display());
        return None;
    }
    let compress: CompressFn = unsafe { get_proc(handle, "headroom_compress")? };
    let retrieve: RetrieveFn = unsafe { get_proc(handle, "headroom_retrieve")? };
    let free: FreeFn = unsafe { get_proc(handle, "headroom_free")? };
    let ccr_stats: CcrStatsFn = unsafe { get_proc(handle, "headroom_ccr_stats")? };
    Some(HeadroomDll {
        compress,
        retrieve,
        free,
        ccr_stats,
    })
}

#[cfg(windows)]
unsafe fn get_proc<T>(handle: *mut std::ffi::c_void, name: &str) -> Option<T> {
    let c_name = std::ffi::CString::new(name).ok()?;
    let ptr = GetProcAddress(handle, c_name.as_ptr() as *const u8);
    if ptr.is_null() {
        None
    } else {
        Some(std::mem::transmute_copy(&ptr))
    }
}

#[cfg(windows)]
extern "system" {
    fn LoadLibraryW(lpFileName: *const u16) -> *mut std::ffi::c_void;
    fn GetProcAddress(
        hModule: *mut std::ffi::c_void,
        lpProcName: *const u8,
    ) -> *mut std::ffi::c_void;
}

#[cfg(not(windows))]
fn load_platform_dll(path: &Path) -> Option<HeadroomDll> {
    use std::ffi::CString;
    let c_path = CString::new(path.to_string_lossy().as_bytes()).ok()?;
    let handle = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW) };
    if handle.is_null() {
        tracing::warn!("dlopen failed for {}", path.display());
        return None;
    }
    unsafe {
        let compress = get_sym(handle, "headroom_compress")?;
        let retrieve = get_sym(handle, "headroom_retrieve")?;
        let free = get_sym(handle, "headroom_free")?;
        let ccr_stats = get_sym(handle, "headroom_ccr_stats")?;
        Some(HeadroomDll {
            compress,
            retrieve,
            free,
            ccr_stats,
        })
    }
}

#[cfg(not(windows))]
unsafe fn get_sym<T>(handle: *mut std::ffi::c_void, name: &str) -> Option<T> {
    let c_name = std::ffi::CString::new(name).ok()?;
    let ptr = libc::dlsym(handle, c_name.as_ptr());
    if ptr.is_null() {
        None
    } else {
        Some(std::mem::transmute_copy(&ptr))
    }
}

// ── DLL search ────────────────────────────────────────────────────

fn find_dll() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("HEADROOM_DLL_PATH") {
        let p = PathBuf::from(&path);
        if p.exists() {
            return Some(p);
        }
    }
    let exe_dir = std::env::current_exe()
        .ok()?
        .parent()
        .map(|p| p.to_path_buf())?;
    for name in &["headroom_core.dll", "headroom_ffi.dll"] {
        let p = exe_dir.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    for name in &["headroom_core.dll", "headroom_ffi.dll"] {
        let p = PathBuf::from(name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}
