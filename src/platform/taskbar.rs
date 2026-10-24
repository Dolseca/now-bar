//! The taskbar we live in. The pill is a child window of `Shell_TrayWnd` (TrafficMonitor-style):
//! a separate topmost popup gets covered whenever the taskbar itself has focus, a child never does.

use std::cell::Cell;
use std::sync::{Arc, Mutex};

use windows::core::{w, BSTR, Result};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationCacheRequest, IUIAutomationElement, SetWinEventHook, TreeScope_Children,
    TreeScope_Descendants, HWINEVENTHOOK, UIA_AutomationIdPropertyId, UIA_BoundingRectanglePropertyId,
};
use windows::Win32::UI::Shell::{SHAppBarMessage, ABM_GETSTATE, ABM_GETTASKBARPOS, APPBARDATA};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::platform::log::log;
use crate::platform::windows::post_taskbar_items;
use crate::render::layout::{RectPx, TaskbarAlign, TaskbarInfo};

pub fn find_taskbar() -> Option<HWND> {
    unsafe { FindWindowW(w!("Shell_TrayWnd"), None).ok() }
}

pub fn window_rect(hwnd: HWND) -> Result<RectPx> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut r)? };
    Ok(RectPx { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top })
}

fn explorer_dword(name: windows::core::PCWSTR) -> Option<u32> {
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced"),
            name,
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    status.is_ok().then_some(value)
}

/// Taskbar rect plus the settings that decide where free space is. Windows 11 defaults
/// (value missing) are centred icons and Widgets on.
pub fn taskbar_info(bar: HWND) -> Result<TaskbarInfo> {
    let align = match explorer_dword(w!("TaskbarAl")) {
        Some(0) => TaskbarAlign::Left,
        _ => TaskbarAlign::Center,
    };
    let widgets = explorer_dword(w!("TaskbarDa")).unwrap_or(1) != 0;
    Ok(TaskbarInfo { rect: window_rect(bar)?, align, widgets, bottom: edge().is_none_or(|e| e == ABE_BOTTOM) })
}

/// Which screen edge the taskbar is on (`None`: the shell didn't say; taken as the bottom,
/// so a failed ask doesn't hide the pill until the next settings change).
fn edge() -> Option<u32> {
    let mut data = APPBARDATA { cbSize: std::mem::size_of::<APPBARDATA>() as u32, ..Default::default() };
    (unsafe { SHAppBarMessage(ABM_GETTASKBARPOS, &mut data) } != 0).then_some(data.uEdge)
}

/// The taskbar hides itself until the mouse comes (then a maximised window covers the monitor).
pub fn auto_hides() -> bool {
    let mut data = APPBARDATA { cbSize: std::mem::size_of::<APPBARDATA>() as u32, ..Default::default() };
    unsafe { SHAppBarMessage(ABM_GETSTATE, &mut data) as u32 & ABS_AUTOHIDE != 0 }
}

const ABE_BOTTOM: u32 = 3;
const ABS_AUTOHIDE: u32 = 1;

/// Makes `child` a child of the taskbar and places it at `rect` (screen coordinates).
pub fn embed(child: HWND, bar: HWND, rect: RectPx) -> Result<()> {
    unsafe {
        // Windows keeps WS_CLIPSIBLINGS here; composition content only shows with it. Because a
        // clip-siblings window is cut out wherever a sibling lies above it, the composition window
        // must be the top-most of our children (see `PillWindow::set_rects`).
        let visible = GetWindowLongPtrW(child, GWL_STYLE) as u32 & WS_VISIBLE.0;
        SetWindowLongPtrW(child, GWL_STYLE, (WS_CHILD.0 | visible) as isize);
        // A top-level window has no previous parent, so SetParent may "fail" with a stale last
        // error even though it worked: check the outcome instead. Right after a previous instance
        // was killed (or Explorer restarted) the taskbar refuses for a moment, so retry briefly.
        let mut attached = false;
        for attempt in 0..30 {
            let _ = SetParent(child, Some(bar));
            if GetParent(child).ok() == Some(bar) {
                attached = true;
                if attempt > 0 {
                    crate::platform::log::log(&format!("embedded into the taskbar after {attempt} retries"));
                }
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if !attached {
            return Err(windows::core::Error::new(windows::Win32::Foundation::E_FAIL, "SetParent into the taskbar failed"));
        }
    }
    place(child, bar, rect)
}

/// Moves an embedded child to `rect` (screen coordinates) and keeps it above its siblings.
pub fn place(child: HWND, bar: HWND, rect: RectPx) -> Result<()> {
    let origin = window_rect(bar)?;
    unsafe {
        SetWindowPos(
            child,
            Some(HWND_TOP),
            rect.x - origin.x,
            rect.y - origin.y,
            rect.w,
            rect.h,
            SWP_NOACTIVATE,
        )
        .inspect_err(|e| crate::platform::log::log(&format!("place {rect:?}: {e:?}")))
    }
}

/// The x spans of the taskbar's own items (Start, Search, the app buttons, the tray...), in
/// screen pixels, for the main thread (told with `post_taskbar_items`).
pub type ItemsMailbox = Arc<Mutex<Vec<(i32, i32)>>>;

/// After a window shows or hides the buttons are read soon, and again once the taskbar's own
/// animation has settled.
const LOOK_SOON_MS: [u64; 2] = [150, 700];
/// And every so often regardless: a renamed or pinned button, a missed event.
const LOOK_EVERY_MS: u64 = 2000;

thread_local! {
    static NUDGED: Cell<bool> = const { Cell::new(false) };
}

extern "system" fn on_window(_: HWINEVENTHOOK, _: u32, hwnd: HWND, object: i32, child: i32, _: u32, _: u32) {
    if object != OBJID_WINDOW.0 || child != CHILDID_SELF as i32 {
        return;
    }
    // Only top-level windows that may have a button: no children, no tool windows.
    unsafe {
        if GetAncestor(hwnd, GA_ROOT) != hwnd || GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
            return;
        }
    }
    NUDGED.with(|n| n.set(true));
}

/// Starts the thread that reads where the taskbar's items are and posts them whenever they
/// change. Windows 11's taskbar is XAML: only UI Automation knows its buttons' rects (the old
/// MSTaskListWClass window is still there, at a stale size).
pub fn watch_items(mailbox: ItemsMailbox) {
    let spawned = std::thread::Builder::new().name("taskbar-items".into()).spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let mut reader = match ItemReader::new() {
            Ok(r) => r,
            Err(e) => return log(&format!("taskbar items: no UI Automation ({e})")),
        };
        // An app opening or closing shows or hides (or, for packaged apps, uncloaks) its window.
        let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
        let _show = SetWinEventHook(EVENT_OBJECT_SHOW, EVENT_OBJECT_HIDE, None, Some(on_window), 0, 0, flags);
        let _cloak = SetWinEventHook(EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED, None, Some(on_window), 0, 0, flags);
        let mut last: Option<Vec<(i32, i32)>> = None;
        let mut soon: Vec<u64> = Vec::new();
        let mut next = GetTickCount64();
        loop {
            let now = GetTickCount64();
            if now >= next || soon.iter().any(|&t| t <= now) {
                soon.retain(|&t| t > now);
                next = now + LOOK_EVERY_MS;
                let items = reader.read();
                if last.as_ref() != Some(&items) {
                    if last.is_none() {
                        log(&format!("taskbar items: {items:?}"));
                    }
                    if let Ok(mut slot) = mailbox.lock() {
                        slot.clone_from(&items);
                    }
                    post_taskbar_items();
                    last = Some(items);
                }
            }
            let due = soon.iter().copied().chain([next]).min().unwrap_or(next);
            let wait = due.saturating_sub(GetTickCount64()).min(u32::MAX as u64) as u32;
            MsgWaitForMultipleObjects(None, false, wait, QS_ALLINPUT);
            // The hooks' callbacks run in here.
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&msg);
            }
            if NUDGED.with(|n| n.replace(false)) && soon.is_empty() {
                let now = GetTickCount64();
                soon = LOOK_SOON_MS.iter().map(|ms| now + ms).collect();
            }
        }
    });
    if let Err(e) = spawned {
        log(&format!("taskbar items: no thread ({e})"));
    }
}

struct ItemReader {
    uia: IUIAutomation,
    cache: IUIAutomationCacheRequest,
    /// The taskbar and its XAML frame (Start, Search, the buttons), found once per taskbar.
    frame: Option<(HWND, IUIAutomationElement)>,
}

impl ItemReader {
    fn new() -> Result<Self> {
        unsafe {
            let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
            let cache = uia.CreateCacheRequest()?;
            cache.AddProperty(UIA_BoundingRectanglePropertyId)?;
            Ok(ItemReader { uia, cache, frame: None })
        }
    }

    /// The items' spans now: the frame's children (one cross-process call, about 6 ms) and the
    /// tray's window. Empty when there is no taskbar.
    fn read(&mut self) -> Vec<(i32, i32)> {
        let Some(bar) = find_taskbar() else {
            self.frame = None;
            return Vec::new();
        };
        let Ok(on) = window_rect(bar) else { return Vec::new() };
        let mut spans = Vec::new();
        if let Some(frame) = self.frame(bar) {
            match self.children(&frame, on) {
                Ok(v) => spans = v,
                // Explorer restarted under the same handle, or the frame was rebuilt.
                Err(_) => self.frame = None,
            }
        }
        if let Ok(tray) = unsafe { FindWindowExW(Some(bar), None, w!("TrayNotifyWnd"), None) } {
            if let Ok(r) = window_rect(tray) {
                spans.push((r.x, r.right()));
            }
        }
        spans
    }

    fn frame(&mut self, bar: HWND) -> Option<IUIAutomationElement> {
        if self.frame.as_ref().is_none_or(|(b, _)| *b != bar) {
            self.frame = unsafe {
                let id = VARIANT::from(BSTR::from("TaskbarFrame"));
                let found = self.uia.CreatePropertyCondition(UIA_AutomationIdPropertyId, &id).and_then(|c| {
                    self.uia.ElementFromHandle(bar)?.FindFirst(TreeScope_Descendants, &c)
                });
                found.ok().map(|f| (bar, f))
            };
        }
        self.frame.as_ref().map(|(_, f)| f.clone())
    }

    /// The frame's children with a size, on the taskbar's row.
    fn children(&self, frame: &IUIAutomationElement, on: RectPx) -> Result<Vec<(i32, i32)>> {
        unsafe {
            let all = frame.FindAllBuildCache(TreeScope_Children, &self.uia.CreateTrueCondition()?, &self.cache)?;
            let mut spans = Vec::new();
            for i in 0..all.Length()? {
                let r = all.GetElement(i)?.CachedBoundingRectangle()?;
                if r.right > r.left && r.bottom > on.y && r.top < on.bottom() {
                    spans.push((r.left, r.right));
                }
            }
            Ok(spans)
        }
    }
}
