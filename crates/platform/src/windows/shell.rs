use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, KF_FLAG_DEFAULT,
    SEE_MASK_FLAG_NO_UI, SEE_MASK_INVOKEIDLIST, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW,
    SHGetKnownFolderItem, SHParseDisplayName, SIGDN, SIGDN_NORMALDISPLAY,
    SIGDN_PARENTRELATIVEPARSING, ShellExecuteExW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, w};

pub struct InstalledApp {
    pub id: String,
    pub name: String,
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
    let name = unsafe { item.GetDisplayName(kind)? };
    let text = unsafe { name.to_string() };
    unsafe { CoTaskMemFree(Some(name.0.cast())) };
    Ok(text?)
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
            apps.push(InstalledApp { id, name });
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
    let path = HSTRING::from(format!("shell:AppsFolder\\{id}"));
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
