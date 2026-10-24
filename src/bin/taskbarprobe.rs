//! Dev tool: lists the taskbar's UI Automation elements with their rects (read-only), to see
//! which ones say where the app buttons end.

use windows::core::Interface;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, FindWindowW, GetClassNameW, GetWindowRect, WindowFromPoint};
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::core::{w, BOOL};

unsafe extern "system" fn child(hwnd: HWND, _: LPARAM) -> BOOL {
    let mut name = [0u16; 128];
    let n = GetClassNameW(hwnd, &mut name);
    let mut r = RECT::default();
    let _ = GetWindowRect(hwnd, &mut r);
    println!("  hwnd {:?} {} ({}, {}) - ({}, {})", hwnd.0, String::from_utf16_lossy(&name[..n as usize]), r.left, r.top, r.right, r.bottom);
    true.into()
}

fn main() -> windows::core::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let bar = FindWindowW(w!("Shell_TrayWnd"), None)?;
        println!("child windows of Shell_TrayWnd:");
        let _ = EnumChildWindows(Some(bar), Some(child), LPARAM(0));
        // Which window the mouse would reach along the bar's middle (our lane is click-through).
        let mut br = RECT::default();
        let _ = GetWindowRect(bar, &mut br);
        for x in (br.left + 30..br.right).step_by(120) {
            let hit = WindowFromPoint(windows::Win32::Foundation::POINT { x, y: (br.top + br.bottom) / 2 });
            let mut name = [0u16; 128];
            let n = GetClassNameW(hit, &mut name);
            println!("  hit at {x}: {}", String::from_utf16_lossy(&name[..n as usize]));
        }
        let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
        for pass in 0..3 {
            let t = std::time::Instant::now();
            let root = uia.ElementFromHandle(bar)?;
            let all = root.FindAll(TreeScope_Descendants, &uia.CreateTrueCondition()?)?;
            let n = all.Length()?;
            let took = t.elapsed();
            println!("pass {pass}: {n} elements in {took:?}");
            if pass > 0 {
                continue;
            }
            for i in 0..n {
                let e = all.GetElement(i)?;
                let r = e.CurrentBoundingRectangle()?;
                let class = e.CurrentClassName().map(|s| s.to_string()).unwrap_or_default();
                let name = e.CurrentName().map(|s| s.to_string()).unwrap_or_default();
                let id = e.CurrentAutomationId().map(|s| s.to_string()).unwrap_or_default();
                let ct = e.CurrentControlType()?.0;
                let fw = e.CurrentFrameworkId().map(|s| s.to_string()).unwrap_or_default();
                let off = e.CurrentIsOffscreen().map(|b| b.as_bool()).unwrap_or_default();
                println!(
                    "{i:3} ct {ct} [{class}] id={id:?} name={name:?} fw={fw} off={off} ({}, {}) - ({}, {})",
                    r.left, r.top, r.right, r.bottom
                );
            }
        }
        // The cached query the app would use: buttons only, rect and class in one round trip.
        let cache = uia.CreateCacheRequest()?;
        cache.AddProperty(UIA_BoundingRectanglePropertyId)?;
        cache.AddProperty(UIA_ClassNamePropertyId)?;
        cache.AddProperty(UIA_AutomationIdPropertyId)?;
        for pass in 0..3 {
            let t = std::time::Instant::now();
            let root = uia.ElementFromHandle(bar)?;
            let cond = uia.CreatePropertyCondition(UIA_ControlTypePropertyId, &windows::Win32::System::Variant::VARIANT::from(UIA_ButtonControlTypeId.0))?;
            let all = root.FindAllBuildCache(TreeScope_Descendants, &cond, &cache)?;
            let n = all.Length()?;
            let mut right = i32::MIN;
            for i in 0..n {
                let e = all.GetElement(i)?;
                let r = e.CachedBoundingRectangle()?;
                right = right.max(r.right);
            }
            println!("cached pass {pass}: {n} buttons, rightmost edge {right}, {:?}", t.elapsed());
        }
        // Only the frame's children, the frame found once.
        let root = uia.ElementFromHandle(bar)?;
        let id = windows::Win32::System::Variant::VARIANT::from(windows::core::BSTR::from("TaskbarFrame"));
        let frame = root.FindFirst(TreeScope_Descendants, &uia.CreatePropertyCondition(UIA_AutomationIdPropertyId, &id)?)?;
        for pass in 0..5 {
            let t = std::time::Instant::now();
            let all = frame.FindAllBuildCache(TreeScope_Children, &uia.CreateTrueCondition()?, &cache)?;
            let n = all.Length()?;
            let mut spans = Vec::new();
            for i in 0..n {
                let e = all.GetElement(i)?;
                let r = e.CachedBoundingRectangle()?;
                spans.push((e.CachedClassName()?.to_string(), r.left, r.right));
            }
            println!("frame children pass {pass}: {n} in {:?}: {spans:?}", t.elapsed());
        }
        let _ = uia.cast::<IUIAutomation>();
    }
    Ok(())
}
