//! Query the window underneath the selector without blocking its input loop.
#![allow(non_upper_case_globals)] // Win32 control type IDs keep their SDK names in patterns.
use super::{intersection, selection_region};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};

static BUSY: AtomicBool = AtomicBool::new(false);
const MAX_ELEMENTS: usize = 4000;

#[derive(Debug)]
pub(super) struct Component {
    bounds: RECT,
    priority: u8,
}

pub(super) fn component_at(components: &[Component], cursor: POINT) -> Option<RECT> {
    components
        .iter()
        .filter(|component| super::contains(component.bounds, cursor))
        .min_by_key(|component| {
            let bounds = component.bounds;
            let area =
                i64::from(bounds.right - bounds.left) * i64::from(bounds.bottom - bounds.top);
            (component.priority, area)
        })
        .map(|component| component.bounds)
}

pub(super) fn request(window: usize, bounds: RECT) -> Option<Receiver<Vec<Component>>> {
    // A slow or hung provider may leave one outstanding call, but never one per mouse move.
    if BUSY.swap(true, Ordering::AcqRel) {
        return None;
    }
    struct Finish;
    impl Drop for Finish {
        fn drop(&mut self) {
            BUSY.store(false, Ordering::Release);
        }
    }
    let finish = Finish;
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("capture-uia".into())
        .spawn(move || {
            let _finish = finish;
            let hwnd = HWND(window as *mut _);
            let _ = sender.send(snapshot(hwnd, bounds).unwrap_or_default());
        })
        .ok()?;
    Some(receiver)
}

fn snapshot(hwnd: HWND, bounds: RECT) -> windows::core::Result<Vec<Component>> {
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let _apartment = Apartment;
        let automation: IUIAutomation =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
        let cache = automation.CreateCacheRequest()?;
        cache.SetTreeScope(TreeScope_Subtree)?;
        cache.SetTreeFilter(&automation.ControlViewCondition()?)?;
        cache.SetAutomationElementMode(AutomationElementMode_None)?;
        cache.AddProperty(UIA_BoundingRectanglePropertyId)?;
        cache.AddProperty(UIA_ControlTypePropertyId)?;
        cache.AddProperty(UIA_IsOffscreenPropertyId)?;
        // Desktop-wide ElementFromPoint would hit our own overlay.
        let root = automation.ElementFromHandleBuildCache(hwnd, &cache)?;
        let mut components = Vec::new();
        let mut remaining = MAX_ELEMENTS;
        collect(&root, bounds, true, 0, &mut remaining, &mut components);
        Ok(components)
    }
}

fn collect(
    element: &IUIAutomationElement,
    clip: RECT,
    root: bool,
    depth: usize,
    remaining: &mut usize,
    components: &mut Vec<Component>,
) {
    if *remaining == 0 || depth == 80 {
        return;
    }
    *remaining -= 1;
    unsafe {
        let kind = element.CachedControlType().unwrap_or_default();
        let mut child_clip = clip;
        if !root {
            if element
                .CachedIsOffscreen()
                .is_ok_and(|value| value.as_bool())
            {
                return;
            }
            if let Ok(rect) = element.CachedBoundingRectangle()
                && rect.left < rect.right
                && rect.top < rect.bottom
            {
                let Some(bounds) = intersection(clip, rect) else {
                    return;
                };
                if selection_region(bounds).is_some()
                    && let Some(priority) = priority(kind)
                {
                    components.push(Component { bounds, priority });
                }
                if matches!(
                    kind,
                    UIA_PaneControlTypeId
                        | UIA_DocumentControlTypeId
                        | UIA_ListControlTypeId
                        | UIA_TableControlTypeId
                        | UIA_DataGridControlTypeId
                        | UIA_TreeControlTypeId
                ) {
                    child_clip = bounds;
                }
            }
        }
        if let Ok(children) = element.GetCachedChildren() {
            for index in 0..children.Length().unwrap_or(0) {
                if *remaining == 0 {
                    break;
                }
                if let Ok(child) = children.GetElement(index) {
                    collect(&child, child_clip, false, depth + 1, remaining, components);
                }
            }
        }
    }
}

fn priority(kind: UIA_CONTROLTYPE_ID) -> Option<u8> {
    match kind {
        UIA_DataItemControlTypeId | UIA_ListItemControlTypeId | UIA_TreeItemControlTypeId => {
            Some(0)
        }
        UIA_ButtonControlTypeId
        | UIA_EditControlTypeId
        | UIA_ComboBoxControlTypeId
        | UIA_CheckBoxControlTypeId
        | UIA_RadioButtonControlTypeId
        | UIA_TabItemControlTypeId
        | UIA_MenuItemControlTypeId
        | UIA_SliderControlTypeId
        | UIA_HyperlinkControlTypeId
        | UIA_HeaderItemControlTypeId
        | UIA_ImageControlTypeId => Some(1),
        UIA_GroupControlTypeId | UIA_ToolBarControlTypeId => Some(2),
        _ => None,
    }
}
