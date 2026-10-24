//! Dev tool: the pill moving aside for the taskbar's buttons, which the PNG sheets can't show. A
//! stand-in taskbar off every screen with grey button squares, the real pill window and visual
//! in it; photographs (`<dir>/glide-<phase>-<ms>.png`) while the buttons grow past the pill, while
//! a wider activity comes in against them (its left edge must stay), while they leave room only
//! for its circle and while they shrink back.
//! Usage: `glidesheet [dir]`.

use nowbar::activities::weather::WeatherFx;
use nowbar::platform::windows::PillWindow;
use nowbar::render::gfx::{draw_surface, rect, Gfx};
use nowbar::render::layout::{fit_pill, pill_rect, RectPx, Scale, TaskbarAlign, TaskbarInfo};
use nowbar::render::pill::{layout_for, PillContent, PillVisual, MAX_SLIVERS, PILL_H_DIP, SLIVER_STEP_DIP};
use windows::core::{w, Interface, Result, HSTRING};
use windows::Win32::Foundation::{GENERIC_WRITE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::{CreateDispatcherQueueController, DispatcherQueueOptions, RoInitialize, DQTAT_COM_NONE, DQTYPE_THREAD_CURRENT, RO_INIT_SINGLETHREADED};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::UI::Composition::{CompositionStretch, Compositor, ContainerVisual};
use windows_numerics::{Vector2, Vector3};

/// Asks DWM for everything, DirectComposition included.
const PW_RENDERFULLCONTENT: u32 = 2;
const BAR: RectPx = RectPx { x: -20_000, y: -20_000, w: 1920, h: 50 };

extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// A taskbar at 125 %: Start, Search, 55 px buttons from 124 px, the tray from 1424 px.
fn items(buttons: i32) -> Vec<(i32, i32)> {
    let mut v = vec![(0, 69), (69, 124)];
    v.extend((0..buttons).map(|i| (124 + 55 * i, 179 + 55 * i)));
    v.push((1424, 1920));
    v.into_iter().map(|(a, b)| (a + BAR.x, b + BAR.x)).collect()
}

/// A grey square per item (the tray one wide strip), like the taskbar's icons.
fn draw_items(c: &Compositor, g: &Gfx, holder: &ContainerVisual, spans: &[(i32, i32)]) -> Result<()> {
    holder.Children()?.RemoveAll()?;
    for &(a, b) in spans {
        let (a, b) = ((a - BAR.x) as f32, (b - BAR.x) as f32);
        let (w, h) = (b - a - 14.0, 36.0);
        let surface = draw_surface(g, w as i32, h as i32, |ink| ink.rrect(rect(0.0, 0.0, w, h), 6.0, [0.38, 0.38, 0.4, 1.0]))?;
        let brush = c.CreateSurfaceBrushWithSurface(&surface)?;
        brush.SetStretch(CompositionStretch::None)?;
        let sprite = c.CreateSpriteVisual()?;
        sprite.SetBrush(&brush)?;
        sprite.SetOffset(Vector3 { X: a + 7.0, Y: 7.0, Z: 0.0 })?;
        sprite.SetSize(Vector2 { X: w, Y: h })?;
        holder.Children()?.InsertAtTop(&sprite)?;
    }
    Ok(())
}

/// Saves the window as DWM composes it.
fn photograph(hwnd: HWND, w: i32, h: i32, out: &str) -> Result<()> {
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let dib = CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let old = SelectObject(mem, dib.into());
        let printed = PrintWindow(hwnd, mem, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)).as_bool();
        // GDI leaves the alpha byte undefined.
        let mut px = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec();
        px.chunks_mut(4).for_each(|p| p[3] = 255);
        SelectObject(mem, old);
        let _ = DeleteObject(dib.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        if !printed {
            return Err(windows::core::Error::from_thread());
        }
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let bmp = wic.CreateBitmapFromMemory(w as u32, h as u32, &GUID_WICPixelFormat32bppBGRA, (w * 4) as u32, &px)?;
        let stream = wic.CreateStream()?;
        stream.InitializeFromFilename(&HSTRING::from(out), GENERIC_WRITE.0)?;
        let encoder = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.expect("png frame");
        frame.Initialize(None)?;
        frame.SetSize(w as u32, h as u32)?;
        let mut format = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut format)?;
        frame.WriteSource(&bmp, std::ptr::null())?;
        frame.Commit()?;
        encoder.Commit()
    }
}

/// Runs the thread's messages (the compositor commits through them) for `ms`.
fn pump(ms: u64) {
    let until = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    let mut msg = MSG::default();
    while std::time::Instant::now() < until {
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
}

/// Photographs every 30 ms for 600 ms from now (the glide lasts 450).
fn film(bar: HWND, dir: &str, phase: &str) -> Result<()> {
    let start = std::time::Instant::now();
    for i in 0..=20 {
        let due = std::time::Duration::from_millis(30 * i);
        pump(due.saturating_sub(start.elapsed()).as_millis() as u64);
        photograph(bar, BAR.w, BAR.h, &format!("{dir}/glide-{phase}-{:03}.png", start.elapsed().as_millis()))?;
    }
    Ok(())
}

fn main() -> Result<()> {
    let dir = std::env::args().nth(1).unwrap_or_else(|| std::env::temp_dir().join("glide").to_string_lossy().into_owned());
    let _ = std::fs::create_dir_all(&dir);
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        RoInitialize(RO_INIT_SINGLETHREADED)?;
    }
    let _queue = unsafe {
        CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_NONE,
        })?
    };
    let c = Compositor::new()?;
    let g = Gfx::new(&c)?;
    let s = Scale(1.25);
    let info = TaskbarInfo { rect: BAR, align: TaskbarAlign::Left, widgets: false, bottom: true };

    let bar = unsafe {
        let instance = GetModuleHandleW(None)?.into();
        let class = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: instance, lpszClassName: w!("NowBarGlideSheet"), ..Default::default() };
        RegisterClassW(&class);
        // Far off every screen: DWM still composes it, nobody sees it.
        CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP, w!("NowBarGlideSheet"), w!("glidesheet"), WS_POPUP | WS_VISIBLE, BAR.x, BAR.y, BAR.w, BAR.h, None, None, Some(instance), None)?
    };
    let target = unsafe { c.cast::<ICompositorDesktopInterop>()?.CreateDesktopWindowTarget(bar, false)? };
    let root = c.CreateContainerVisual()?;
    root.SetSize(Vector2 { X: BAR.w as f32, Y: BAR.h as f32 })?;
    let backdrop = c.CreateSpriteVisual()?;
    backdrop.SetBrush(&c.CreateColorBrushWithColor(windows::UI::Color { A: 255, R: 0x20, G: 0x1A, B: 0x24 })?)?;
    backdrop.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
    root.Children()?.InsertAtBottom(&backdrop)?;
    let buttons = c.CreateContainerVisual()?;
    root.Children()?.InsertAtTop(&buttons)?;
    target.SetRoot(&root)?;

    let timer = PillContent::Timer { text: "4:59".into(), label: "Zamanlayıcı", paused: false };
    let weather = PillContent::Weather { symbol: "⛅".into(), temp: "17°".into(), desc: "Parçalı bulutlu".into(), fx: WeatherFx::Clouds };
    let tail = s.px(SLIVER_STEP_DIP * MAX_SLIVERS as f32) as i32;
    let gap = s.px(8.0) as i32;
    let spot = |front: &PillContent, spans: &[(i32, i32)]| {
        let home = pill_rect(&info, front.width_dip(), PILL_H_DIP, &s, 0.0);
        let circle = pill_rect(&info, PILL_H_DIP, PILL_H_DIP, &s, 0.0);
        fit_pill(home, RectPx { w: circle.h, ..circle }, tail, gap, &info, spans).expect("room")
    };

    let lane = RectPx { x: BAR.x, w: BAR.w, ..pill_rect(&info, timer.width_dip(), PILL_H_DIP, &s, 0.0) };
    let window = PillWindow::create(&g, bar, lane, layout_for(&timer, &s).frame_w)?;
    let mut pill = PillVisual::new(&g, &window.stage)?;

    // Ten buttons: the timer pill at home.
    let spans = items(10);
    draw_items(&c, &g, &buttons, &spans)?;
    let l = pill.show(&g, &timer, &[], &s, false, false)?;
    window.place(spot(&timer, &spans).0, l.frame_w, l.radius, false)?;
    window.set_visible(true);
    pump(400);
    photograph(bar, BAR.w, BAR.h, &format!("{dir}/glide-a-home.png"))?;

    // Fifteen: they reach the pill, which glides right of them.
    let spans = items(15);
    draw_items(&c, &g, &buttons, &spans)?;
    window.place(spot(&timer, &spans).0, l.frame_w, l.radius, window.shown())?;
    film(bar, &dir, "b-pushed")?;

    // A wider activity against the buttons: it grows to the right only.
    let l = pill.show(&g, &weather, &[], &s, true, false)?;
    window.place(spot(&weather, &spans).0, l.frame_w, l.radius, window.shown())?;
    film(bar, &dir, "c-wider")?;

    // Nineteen: room for the circle only.
    let spans = items(19);
    draw_items(&c, &g, &buttons, &spans)?;
    let (r, compact) = spot(&weather, &spans);
    let l = pill.show(&g, &weather, &[], &s, false, compact)?;
    window.place(r, l.frame_w, l.radius, window.shown())?;
    film(bar, &dir, "d-circle")?;

    // Fifteen: the whole pill again.
    let spans = items(15);
    draw_items(&c, &g, &buttons, &spans)?;
    let (r, compact) = spot(&weather, &spans);
    let l = pill.show(&g, &weather, &[], &s, false, compact)?;
    window.place(r, l.frame_w, l.radius, window.shown())?;
    film(bar, &dir, "e-whole")?;

    // Back to ten: home again.
    let spans = items(10);
    draw_items(&c, &g, &buttons, &spans)?;
    window.place(spot(&weather, &spans).0, l.frame_w, l.radius, window.shown())?;
    film(bar, &dir, "f-back")?;
    println!("{dir}");
    Ok(())
}
