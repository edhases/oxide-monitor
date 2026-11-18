//! Lazy Win32 popup host (`OxideFlyout`) for the extended Win11 Flyout card.
//!
//! Creates its self-contained `FlyoutRenderer` (CPU antialiased shapes + GDI ClearType DIB,
//! zero Direct2D/WARP threads) only when opened, and destroys both the renderer and hidden
//! HWND 5 seconds after hiding so the idle footprint returns to baseline.

use std::time::Instant;

use oxide_core::{Config, HistoryStore, Localizer, Snapshot};
use oxide_render::{
    FlyoutButton, FlyoutHitZone, FlyoutInput, FlyoutRenderer, SystemThemeContext,
    present_layered_window,
};
use tracing::{debug, warn};
use windows::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetCursorPos, GetWindowLongPtrW,
    HWND_TOPMOST, IDC_ARROW, IsWindow, LoadCursorW, MA_NOACTIVATE, PostMessageW, RegisterClassW,
    SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, WM_APP, WM_ERASEBKGND, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCDESTROY,
    WM_PAINT, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::geometry::{RectI, TaskbarGeometry, compute_flyout_position};
use crate::per_core::PerCoreSampler;

const FLYOUT_CLASS: PCWSTR = w!("OxideFlyout");
const WM_MOUSELEAVE: u32 = 0x02A3;

/// Posted to the owner overlay HWND when a button inside the Flyout is clicked.
pub const WM_OXIDE_FLYOUT_BUTTON: u32 = WM_APP + 25;
/// Posted to the owner overlay HWND when the mouse leaves the Flyout window.
pub const WM_OXIDE_FLYOUT_LEAVE: u32 = WM_APP + 26;

pub const BTN_TOGGLE_GAME_MODE: usize = 1;
pub const BTN_OPEN_SETTINGS: usize = 2;
pub const BTN_OPEN_TASK_MGR: usize = 3;

struct FlyoutWndState {
    owner_hwnd: HWND,
    hit_zones: Vec<FlyoutHitZone>,
    mouse_inside: bool,
    tracking_leave: bool,
}

/// Manages the lifecycle, lazy rendering, and idle memory release of the Flyout card.
pub struct FlyoutHost {
    hwnd: Option<HWND>,
    wnd_state: Option<Box<FlyoutWndState>>,
    renderer: Option<FlyoutRenderer>,
    per_core: PerCoreSampler,
    visible: bool,
    pinned: bool,
    hidden_since: Option<Instant>,
    last_rect: RectI,
}

impl FlyoutHost {
    pub fn new() -> Self {
        Self {
            hwnd: None,
            wnd_state: None,
            renderer: None,
            per_core: PerCoreSampler::new(),
            visible: false,
            pinned: false,
            hidden_since: None,
            last_rect: RectI::default(),
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn is_pinned(&self) -> bool {
        self.pinned
    }

    pub fn is_cursor_inside(&self) -> bool {
        if !self.visible {
            return false;
        }
        if self.wnd_state.as_ref().is_some_and(|s| s.mouse_inside) {
            return true;
        }
        let mut pt = POINT::default();
        if unsafe { GetCursorPos(&mut pt) }.is_ok() {
            return pt.x >= self.last_rect.left
                && pt.x < self.last_rect.right
                && pt.y >= self.last_rect.top
                && pt.y < self.last_rect.bottom;
        }
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        owner_hwnd: HWND,
        overlay_rect: RectI,
        geom: &TaskbarGeometry,
        config: &Config,
        snapshot: &Snapshot,
        history: &HistoryStore,
        theme_ctx: &SystemThemeContext,
        localizer: &Localizer,
        game_mode_active: bool,
        pinned: bool,
    ) {
        if !config.flyout.enabled {
            return;
        }
        if !self.ensure_window(owner_hwnd) {
            return;
        }
        self.pinned = pinned;
        self.visible = true;
        self.hidden_since = None;

        self.refresh(
            overlay_rect,
            geom,
            config,
            snapshot,
            history,
            theme_ctx,
            localizer,
            game_mode_active,
        );

        if let Some(hwnd) = self.hwnd {
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    self.last_rect.left,
                    self.last_rect.top,
                    self.last_rect.width(),
                    self.last_rect.height(),
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
        }
    }

    pub fn hide(&mut self) {
        if !self.visible {
            return;
        }
        self.visible = false;
        self.pinned = false;
        self.hidden_since = Some(Instant::now());
        self.per_core.clear();
        if let Some(state) = &mut self.wnd_state {
            state.mouse_inside = false;
            state.tracking_leave = false;
        }
        if let Some(hwnd) = self.hwnd {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }

    /// Release the Flyout's DIB surface and GDI fonts AND destroy the hidden
    /// `OxideFlyout` HWND once hidden for >= 5 seconds so Windows/DWM immediately reclaims
    /// the layered redirection bitmap as well (0 bytes idle footprint, 0 extra threads).
    pub fn release_if_idle(&mut self) {
        if self.visible {
            return;
        }
        if let Some(since) = self.hidden_since
            && since.elapsed().as_secs() >= 5
            && (self.renderer.is_some() || self.hwnd.is_some())
        {
            self.renderer = None;
            if let Some(hwnd) = self.hwnd.take()
                && unsafe { IsWindow(Some(hwnd)) }.as_bool()
            {
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    let _ = DestroyWindow(hwnd);
                }
            }
            self.wnd_state = None;
            self.hidden_since = None;
            debug!("released idle Flyout DIB and destroyed hidden Flyout HWND");
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn refresh(
        &mut self,
        overlay_rect: RectI,
        geom: &TaskbarGeometry,
        config: &Config,
        snapshot: &Snapshot,
        history: &HistoryStore,
        theme_ctx: &SystemThemeContext,
        localizer: &Localizer,
        game_mode_active: bool,
    ) {
        if !self.visible {
            return;
        }
        let Some(hwnd) = self.hwnd else {
            return;
        };

        if self.renderer.is_none() {
            match FlyoutRenderer::new() {
                Ok(r) => self.renderer = Some(r),
                Err(err) => {
                    warn!(%err, "failed to initialize Flyout renderer");
                    self.hide();
                    return;
                }
            }
        }

        let per_core_slice = if config.flyout.show_cpu_per_core {
            self.per_core.poll()
        } else {
            &[]
        };

        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };

        let input = FlyoutInput {
            config,
            snapshot,
            history,
            per_core_pct: per_core_slice,
            theme_ctx,
            localizer,
            game_mode_active,
            dpi: geom.dpi,
        };

        match renderer.render(&input) {
            Ok(frame) => {
                let (w, h) = (frame.width_px as i32, frame.height_px as i32);
                let (x, y) = compute_flyout_position(overlay_rect, w, h, geom);
                self.last_rect = RectI::new(x, y, x + w, y + h);
                if let Some(state) = &mut self.wnd_state {
                    state.hit_zones = frame.hit_zones;
                }
                if let Err(err) = present_layered_window(hwnd, renderer.dib(), Some((x, y)), 255) {
                    warn!(%err, "present_layered_window(flyout) failed");
                }
            }
            Err(err) => warn!(%err, "Flyout render failed"),
        }
    }

    fn ensure_window(&mut self, owner_hwnd: HWND) -> bool {
        if let Some(hwnd) = self.hwnd
            && unsafe { IsWindow(Some(hwnd)) }.as_bool()
        {
            return true;
        }

        let Ok(module) = (unsafe { GetModuleHandleW(PCWSTR::null()) }) else {
            return false;
        };
        let instance = HINSTANCE(module.0);
        if !register_flyout_class(instance) {
            return false;
        }

        let Ok(hwnd) = (unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
                FLYOUT_CLASS,
                w!("Oxide Monitor Details"),
                WS_POPUP,
                0,
                0,
                380,
                320,
                None,
                None,
                Some(instance),
                None,
            )
        }) else {
            warn!("CreateWindowExW(OxideFlyout) failed");
            return false;
        };

        let mut state = Box::new(FlyoutWndState {
            owner_hwnd,
            hit_zones: Vec::new(),
            mouse_inside: false,
            tracking_leave: false,
        });
        unsafe {
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                &mut *state as *mut FlyoutWndState as isize,
            );
        }
        self.wnd_state = Some(state);
        self.hwnd = Some(hwnd);
        true
    }
}

impl Drop for FlyoutHost {
    fn drop(&mut self) {
        if let Some(hwnd) = self.hwnd.take()
            && unsafe { IsWindow(Some(hwnd)) }.as_bool()
        {
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let _ = DestroyWindow(hwnd);
            }
        }
        self.renderer = None;
        self.wnd_state = None;
    }
}

fn register_flyout_class(instance: HINSTANCE) -> bool {
    let wc = WNDCLASSW {
        lpfnWndProc: Some(flyout_wndproc),
        hInstance: instance,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        lpszClassName: FLYOUT_CLASS,
        ..Default::default()
    };
    if unsafe { RegisterClassW(&wc) } == 0 {
        let err = unsafe { GetLastError() };
        if err != ERROR_CLASS_ALREADY_EXISTS {
            warn!("RegisterClassW(OxideFlyout) failed: {}", err.0);
            return false;
        }
    }
    true
}

unsafe extern "system" fn flyout_wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut FlyoutWndState };
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }

    match message {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe {
                let _ = BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            let state = unsafe { &mut *ptr };
            state.mouse_inside = true;
            if !state.tracking_leave {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                if unsafe { TrackMouseEvent(&mut tme) }.is_ok() {
                    state.tracking_leave = true;
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            let state = unsafe { &mut *ptr };
            state.mouse_inside = false;
            state.tracking_leave = false;
            let owner = state.owner_hwnd;
            unsafe {
                let _ = PostMessageW(Some(owner), WM_OXIDE_FLYOUT_LEAVE, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let state = unsafe { &*ptr };
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            for hz in &state.hit_zones {
                if hz.contains(x, y) {
                    let code = match hz.button {
                        FlyoutButton::ToggleGameMode => BTN_TOGGLE_GAME_MODE,
                        FlyoutButton::OpenSettings => BTN_OPEN_SETTINGS,
                        FlyoutButton::OpenTaskManager => BTN_OPEN_TASK_MGR,
                    };
                    unsafe {
                        let _ = PostMessageW(
                            Some(state.owner_hwnd),
                            WM_OXIDE_FLYOUT_BUTTON,
                            WPARAM(code),
                            LPARAM(0),
                        );
                    }
                    break;
                }
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
