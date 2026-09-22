use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ, RegCloseKey, RegDeleteValueW, RegOpenKeyExW,
    RegQueryValueExW, RegSetValueExW,
};
use windows::core::{PCWSTR, w};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: PCWSTR = w!("NextGestures");

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn is_enabled() -> bool {
    unsafe {
        let mut hkey = Default::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, KEY_READ, &mut hkey) != ERROR_SUCCESS {
            return false;
        }
        let ok = RegQueryValueExW(hkey, VALUE_NAME, None, None, None, None) == ERROR_SUCCESS;
        let _ = RegCloseKey(hkey);
        ok
    }
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    unsafe {
        let mut hkey = Default::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, KEY_WRITE, &mut hkey) != ERROR_SUCCESS {
            return Err("打开注册表项失败".into());
        }
        let result = if enabled {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let value = to_wide(&format!("\"{}\"", exe.display()));
            let bytes = std::slice::from_raw_parts(value.as_ptr() as *const u8, value.len() * 2);
            RegSetValueExW(hkey, VALUE_NAME, None, REG_SZ, Some(bytes))
        } else {
            RegDeleteValueW(hkey, VALUE_NAME)
        };
        let _ = RegCloseKey(hkey);
        if result != ERROR_SUCCESS {
            return Err("写入注册表失败".into());
        }
    }
    Ok(())
}
