use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::path::Path;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, IShellItem2, KF_FLAG_DEFAULT,
    SEE_MASK_FLAG_NO_UI, SEE_MASK_INVOKEIDLIST, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW,
    SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_PIDL, SHGetFileInfoW, SHGetIDListFromObject,
    SHGetKnownFolderItem, SHParseDisplayName, SIGDN, SIGDN_FILESYSPATH, SIGDN_NORMALDISPLAY,
    SIGDN_PARENTRELATIVEPARSING, ShellExecuteExW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{GUID, HSTRING, Interface, PCWSTR, PWSTR, w};

pub struct InstalledApp {
    pub id: String,
    pub name: String,
    pub path: String,
    pub icon: Option<String>,
}

struct ShellApartment;

impl ShellApartment {
    fn init() -> Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
        Ok(Self)
    }
}

impl Drop for ShellApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

fn item_name(item: &IShellItem, kind: SIGDN) -> Result<String> {
    shell_string(unsafe { item.GetDisplayName(kind)? })
}

fn shell_string(name: PWSTR) -> Result<String> {
    if name.is_null() {
        bail!("Windows Shell 返回了空字符串指针");
    }
    let text = unsafe { name.to_string() };
    unsafe { CoTaskMemFree(Some(name.0.cast())) };
    Ok(text?)
}

fn apps_folder_path(id: &str) -> String {
    format!("shell:AppsFolder\\{id}")
}

fn app_path(item: &IShellItem, id: &str) -> String {
    // PKEY_Link_TargetParsingPath, e.g. the EXE behind a Start menu entry.
    const TARGET: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0xb9b4b3fc_2b51_4a42_b5d8_324146afcf25),
        pid: 2,
    };
    let link = item.cast::<IShellItem2>().ok()
        .and_then(|item| unsafe { item.GetString(&TARGET) }.ok())
        .and_then(|name| shell_string(name).ok());
    link.filter(|path| Path::new(path).is_absolute())
        .or_else(|| item_name(item, SIGDN_FILESYSPATH).ok().filter(|path| Path::new(path).is_absolute()))
        .or_else(|| Path::new(id).is_absolute().then(|| id.to_owned()))
        .unwrap_or_else(|| apps_folder_path(id))
}

fn app_icon(item: &IShellItem) -> Option<String> {
    // A Shell item can have an icon even when it has no filesystem path.
    let pidl = unsafe { SHGetIDListFromObject(item) }.ok()?;
    let mut info = SHFILEINFOW::default();
    let result = unsafe {
        let result = SHGetFileInfoW(
            PCWSTR(pidl.cast()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_PIDL | SHGFI_LARGEICON,
        );
        CoTaskMemFree(Some(pidl.cast()));
        result
    };
    if result == 0 {
        return None;
    }
    super::window::owned_icon_data_url(info.hIcon)
}

/// Enumerate the Windows AppsFolder, including desktop and Microsoft Store apps.
pub fn installed_apps() -> Result<Vec<InstalledApp>> {
    let _apartment = ShellApartment::init()?;
    let folder: IShellItem = unsafe { SHGetKnownFolderItem(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None)? };
    let items: IEnumShellItems = unsafe { folder.BindToHandler(None, &BHID_EnumItems)? };
    let mut apps = Vec::new();
    let mut seen = HashSet::new();
    loop {
        let mut next = [None];
        unsafe { items.Next(&mut next, None)? };
        let [Some(item)] = next else { break };
        if let (Ok(name), Ok(id)) = (
            item_name(&item, SIGDN_NORMALDISPLAY),
            item_name(&item, SIGDN_PARENTRELATIVEPARSING),
        ) && !name.is_empty()
            && !id.is_empty()
            && seen.insert(id.to_lowercase())
        {
            let path = app_path(&item, &id);
            let icon = app_icon(&item);
            apps.push(InstalledApp { id, name, path, icon });
        }
    }
    Ok(apps)
}

/// Resolve the saved AppsFolder entry and run its Shell open verb.
pub fn open_installed_app(id: &str) -> Result<()> {
    if id.trim().is_empty() || id.contains('\0') {
        bail!("无效的应用标识");
    }
    let _apartment = ShellApartment::init()?;
    let path = HSTRING::from(apps_folder_path(id));
    let mut pidl = std::ptr::null_mut();
    unsafe { SHParseDisplayName(&path, None, &mut pidl, 0, None) }
        .context("找不到该应用")?;
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: w!("open"),
        nShow: SW_SHOWNORMAL.0,
        lpIDList: pidl.cast(),
        ..Default::default()
    };
    let result = unsafe { ShellExecuteExW(&mut info) };
    unsafe { CoTaskMemFree(Some(pidl.cast())) };
    Ok(result?)
}

/// 用系统默认程序打开 URL
pub fn open_url(url: &str) -> Result<()> {
    let target = HSTRING::from(url);
    let r = unsafe { ShellExecuteW(None, w!("open"), &target, None, None, SW_SHOWNORMAL) };
    if r.0 as isize <= 32 {
        bail!("打开链接失败: {url}");
    }
    Ok(())
}
