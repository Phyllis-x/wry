//! Win32 probes and real input injection used by the verification scenarios.

use std::{
  collections::{HashMap, HashSet},
  ffi::c_void,
  mem::size_of,
  sync::atomic::{AtomicBool, AtomicIsize, Ordering},
  thread,
  time::Duration,
};

use serde::Serialize;
use windows::{
  Win32::{
    Foundation::{CloseHandle, HANDLE, HWND, LPARAM, POINT, RECT},
    Graphics::{
      Dwm::{
        DWMWA_CLOAK, DWMWA_CLOAKED, DWMWA_TRANSITIONS_FORCEDISABLED, DwmGetWindowAttribute,
        DwmSetWindowAttribute,
      },
      Gdi::{ClientToScreen, GetDC, GetPixel, ReleaseDC},
    },
    System::{
      Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx},
      Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
      },
      Threading::{
        GR_GDIOBJECTS, GR_USEROBJECTS, GetCurrentProcess, GetCurrentProcessId, GetGuiResources,
        GetProcessHandleCount, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
      },
    },
    UI::{
      Accessibility::{CUIAutomation, IUIAutomation},
      Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
        KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE,
        MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE,
        MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, MapVirtualKeyW, SendInput,
        VIRTUAL_KEY, VK_ESCAPE,
      },
      WindowsAndMessaging::{
        EnumChildWindows, EnumWindows, GA_ROOT, GUITHREADINFO, GW_OWNER, GWL_EXSTYLE, GWL_STYLE,
        GetAncestor, GetClassNameW, GetCursorPos, GetForegroundWindow, GetGUIThreadInfo, GetPropW,
        GetSystemMetrics, GetWindow, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId,
        IsWindow, IsWindowVisible, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        SetWindowLongPtrW, SetWindowPos, WS_CAPTION, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
        WS_EX_TOPMOST, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU, WS_THICKFRAME,
        WindowFromPoint,
      },
    },
  },
  core::{BOOL, PWSTR, Result, w},
};

pub fn hwnd(raw: isize) -> HWND {
  HWND(raw as *mut c_void)
}

pub fn is_window(hwnd: HWND) -> bool {
  unsafe { IsWindow(Some(hwnd)).as_bool() }
}

/// Gives the menu window the styles PinPaper ARCHITECTURE 4.5.1 lists that Tauri does not expose
/// (`WS_POPUP`, `WS_EX_TOOLWINDOW`), and turns off the DWM show/hide transition.
pub fn apply_menu_styles(hwnd: HWND) -> Result<()> {
  unsafe {
    let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
    let style = (style
      & !(WS_CAPTION | WS_THICKFRAME | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX).0)
      | WS_POPUP.0;
    SetWindowLongPtrW(hwnd, GWL_STYLE, style as isize);
    let ex_style =
      (GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & !WS_EX_APPWINDOW.0) | WS_EX_TOOLWINDOW.0;
    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex_style as isize);
    SetWindowPos(
      hwnd,
      None,
      0,
      0,
      0,
      0,
      SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
    )?;
    let disabled = BOOL::from(true);
    DwmSetWindowAttribute(
      hwnd,
      DWMWA_TRANSITIONS_FORCEDISABLED,
      &disabled as *const BOOL as *const c_void,
      size_of::<BOOL>() as u32,
    )
  }
}

#[derive(Debug, Serialize)]
pub struct Styles {
  pub style: String,
  pub ex_style: String,
  pub popup: bool,
  pub tool_window: bool,
  pub topmost: bool,
  pub no_redirection_bitmap: bool,
}

pub fn styles(hwnd: HWND) -> Styles {
  const WS_EX_NOREDIRECTIONBITMAP: u32 = 0x0020_0000;
  let (style, ex_style) = unsafe {
    (
      GetWindowLongPtrW(hwnd, GWL_STYLE) as u32,
      GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32,
    )
  };
  Styles {
    style: format!("{style:#010x}"),
    ex_style: format!("{ex_style:#010x}"),
    popup: style & WS_POPUP.0 != 0,
    tool_window: ex_style & WS_EX_TOOLWINDOW.0 != 0,
    topmost: ex_style & WS_EX_TOPMOST.0 != 0,
    no_redirection_bitmap: ex_style & WS_EX_NOREDIRECTIONBITMAP != 0,
  }
}

pub fn set_cloak(hwnd: HWND, cloak: bool) -> Result<()> {
  let value = BOOL::from(cloak);
  unsafe {
    DwmSetWindowAttribute(
      hwnd,
      DWMWA_CLOAK,
      &value as *const BOOL as *const c_void,
      size_of::<BOOL>() as u32,
    )
  }
}

/// `DWMWA_CLOAKED`: 0 when visible, `DWM_CLOAKED_APP` (1) after `DWMWA_CLOAK`.
pub fn cloaked(hwnd: HWND) -> u32 {
  let mut value = 0u32;
  let _ = unsafe {
    DwmGetWindowAttribute(
      hwnd,
      DWMWA_CLOAKED,
      &mut value as *mut u32 as *mut c_void,
      size_of::<u32>() as u32,
    )
  };
  value
}

pub fn client_to_screen(hwnd: HWND, x: i32, y: i32) -> (i32, i32) {
  let mut point = POINT { x, y };
  let _ = unsafe { ClientToScreen(hwnd, &mut point) };
  (point.x, point.y)
}

/// Reads one pixel of the composed desktop.
pub fn screen_pixel((x, y): (i32, i32)) -> (u8, u8, u8) {
  unsafe {
    let hdc = GetDC(None);
    let color = GetPixel(hdc, x, y).0;
    ReleaseDC(None, hdc);
    (
      (color & 0xff) as u8,
      ((color >> 8) & 0xff) as u8,
      ((color >> 16) & 0xff) as u8,
    )
  }
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Resources {
  pub handles: u32,
  pub gdi: u32,
  pub user: u32,
}

impl Resources {
  fn of(process: HANDLE) -> Self {
    let mut handles = 0;
    unsafe {
      let _ = GetProcessHandleCount(process, &mut handles);
      Self {
        handles,
        gdi: GetGuiResources(process, GR_GDIOBJECTS),
        user: GetGuiResources(process, GR_USEROBJECTS),
      }
    }
  }

  fn add(&mut self, other: Self) {
    self.handles += other.handles;
    self.gdi += other.gdi;
    self.user += other.user;
  }
}

#[derive(Debug, Serialize)]
pub struct Snapshot {
  pub demo: Resources,
  pub webview2: Resources,
  pub webview2_processes: usize,
}

/// Handle and GDI/USER object counts of this process and of every WebView2 process it spawned.
pub fn snapshot() -> Snapshot {
  let demo = Resources::of(unsafe { GetCurrentProcess() });
  let mut webview2 = Resources::default();
  let pids = descendants(unsafe { GetCurrentProcessId() }, "msedgewebview2.exe");
  for &pid in &pids {
    if let Ok(process) = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
      webview2.add(Resources::of(process));
      let _ = unsafe { CloseHandle(process) };
    }
  }
  Snapshot {
    demo,
    webview2,
    webview2_processes: pids.len(),
  }
}

fn descendants(root: u32, exe: &str) -> Vec<u32> {
  let mut children: HashMap<u32, Vec<(u32, String)>> = HashMap::new();
  unsafe {
    let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
      return Vec::new();
    };
    let mut entry = PROCESSENTRY32W {
      dwSize: size_of::<PROCESSENTRY32W>() as u32,
      ..Default::default()
    };
    let mut ok = Process32FirstW(snapshot, &mut entry).is_ok();
    while ok {
      let len = entry
        .szExeFile
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(entry.szExeFile.len());
      let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
      children
        .entry(entry.th32ParentProcessID)
        .or_default()
        .push((entry.th32ProcessID, name));
      ok = Process32NextW(snapshot, &mut entry).is_ok();
    }
    let _ = CloseHandle(snapshot);
  }
  let mut found = Vec::new();
  let mut seen = HashSet::from([root]);
  let mut queue = vec![root];
  while let Some(pid) = queue.pop() {
    for (child, name) in children.get(&pid).into_iter().flatten() {
      if seen.insert(*child) {
        if name.eq_ignore_ascii_case(exe) {
          found.push(*child);
        }
        queue.push(*child);
      }
    }
  }
  found
}

static GUARDED_HOST: AtomicIsize = AtomicIsize::new(0);
static BLOCKED: AtomicBool = AtomicBool::new(false);

/// From now on, clicks and wheel notches are only injected while the cursor is over `host`, and
/// keys only while `host` is the foreground window. Once a check fails, all injection stops, so
/// nothing can land in whatever else is on screen.
pub fn guard_input_to(host: HWND) {
  GUARDED_HOST.store(host.0 as isize, Ordering::SeqCst);
}

pub fn input_blocked() -> bool {
  BLOCKED.load(Ordering::SeqCst)
}

/// Turns all injection, cursor moves included, into no-ops.
pub fn disable_input() {
  BLOCKED.store(true, Ordering::SeqCst);
}

fn allowed(target: HWND, what: &str) -> bool {
  let host = GUARDED_HOST.load(Ordering::SeqCst);
  let root = unsafe { GetAncestor(target, GA_ROOT) };
  if BLOCKED.load(Ordering::SeqCst) || host == 0 || root.0 as isize != host {
    if !BLOCKED.swap(true, Ordering::SeqCst) {
      eprintln!(
        "input guard: {what} would reach {:#x} ({}), not the menu window; injection stopped",
        root.0 as isize,
        exe_of_window(root)
      );
    }
    return false;
  }
  true
}

fn cursor_allowed(what: &str) -> bool {
  let mut point = POINT::default();
  let _ = unsafe { GetCursorPos(&mut point) };
  allowed(unsafe { WindowFromPoint(point) }, what)
}

fn foreground_allowed(what: &str) -> bool {
  allowed(unsafe { GetForegroundWindow() }, what)
}

fn send(inputs: &[INPUT]) {
  unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
}

fn mouse(dx: i32, dy: i32, data: i32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
  INPUT {
    r#type: INPUT_MOUSE,
    Anonymous: INPUT_0 {
      mi: MOUSEINPUT {
        dx,
        dy,
        mouseData: data as u32,
        dwFlags: flags,
        time: 0,
        dwExtraInfo: 0,
      },
    },
  }
}

/// Moves the real cursor to a screen point, as a physical mouse would.
pub fn move_to((x, y): (i32, i32)) {
  if input_blocked() {
    return;
  }
  let (vx, vy, vw, vh) = unsafe {
    (
      GetSystemMetrics(SM_XVIRTUALSCREEN),
      GetSystemMetrics(SM_YVIRTUALSCREEN),
      GetSystemMetrics(SM_CXVIRTUALSCREEN),
      GetSystemMetrics(SM_CYVIRTUALSCREEN),
    )
  };
  let nx = ((x - vx) as i64 * 65535 / (vw - 1).max(1) as i64) as i32;
  let ny = ((y - vy) as i64 * 65535 / (vh - 1).max(1) as i64) as i32;
  send(&[mouse(
    nx,
    ny,
    0,
    MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
  )]);
  thread::sleep(Duration::from_millis(30));
}

/// Moves in a few steps so hover transitions see intermediate positions.
pub fn glide((x0, y0): (i32, i32), (x1, y1): (i32, i32)) {
  for step in 1..=8 {
    move_to((x0 + (x1 - x0) * step / 8, y0 + (y1 - y0) * step / 8));
  }
}

pub fn left_click() {
  if !cursor_allowed("a click") {
    return;
  }
  send(&[mouse(0, 0, 0, MOUSEEVENTF_LEFTDOWN)]);
  thread::sleep(Duration::from_millis(40));
  send(&[mouse(0, 0, 0, MOUSEEVENTF_LEFTUP)]);
  thread::sleep(Duration::from_millis(60));
}

pub fn left_down() {
  if !cursor_allowed("a button press") {
    return;
  }
  send(&[mouse(0, 0, 0, MOUSEEVENTF_LEFTDOWN)]);
  thread::sleep(Duration::from_millis(40));
}

/// Always let through, wherever the cursor is, so the button is never left held down. If an OLE
/// drag-and-drop loop has taken the capture from the menu window, it is cancelled first, so the
/// release cannot drop anything on another window.
pub fn left_up() {
  let mut info = GUITHREADINFO {
    cbSize: size_of::<GUITHREADINFO>() as u32,
    ..Default::default()
  };
  let _ = unsafe { GetGUIThreadInfo(0, &mut info) };
  let capture = info.hwndCapture.0 as isize;
  if capture != 0 && capture != GUARDED_HOST.load(Ordering::SeqCst) {
    tap(VK_ESCAPE);
  }
  send(&[mouse(0, 0, 0, MOUSEEVENTF_LEFTUP)]);
  thread::sleep(Duration::from_millis(60));
}

pub fn window_origin(hwnd: HWND) -> (i32, i32) {
  let mut rect = RECT::default();
  let _ = unsafe { GetWindowRect(hwnd, &mut rect) };
  (rect.left, rect.top)
}

pub fn move_window(hwnd: HWND, (x, y): (i32, i32)) -> Result<()> {
  unsafe {
    SetWindowPos(
      hwnd,
      None,
      x,
      y,
      0,
      0,
      SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
    )
  }
}

/// One wheel notch is 120; negative scrolls down (towards the user).
pub fn wheel(delta: i32) {
  if !cursor_allowed("a wheel notch") {
    return;
  }
  send(&[mouse(0, 0, delta, MOUSEEVENTF_WHEEL)]);
  thread::sleep(Duration::from_millis(60));
}

pub fn hwheel(delta: i32) {
  if !cursor_allowed("a horizontal wheel notch") {
    return;
  }
  send(&[mouse(0, 0, delta, MOUSEEVENTF_HWHEEL)]);
  thread::sleep(Duration::from_millis(60));
}

fn key(vk: VIRTUAL_KEY, up: bool) {
  // A key-up is always let through, so no key is left held down.
  if !up && !foreground_allowed("a key press") {
    return;
  }
  let scan = unsafe { MapVirtualKeyW(vk.0 as u32, MAPVK_VK_TO_VSC) } as u16;
  send(&[INPUT {
    r#type: INPUT_KEYBOARD,
    Anonymous: INPUT_0 {
      ki: KEYBDINPUT {
        wVk: vk,
        wScan: scan,
        dwFlags: if up {
          KEYEVENTF_KEYUP
        } else {
          KEYBD_EVENT_FLAGS(0)
        },
        time: 0,
        dwExtraInfo: 0,
      },
    },
  }]);
}

pub fn tap(vk: VIRTUAL_KEY) {
  key(vk, false);
  thread::sleep(Duration::from_millis(20));
  key(vk, true);
  thread::sleep(Duration::from_millis(40));
}

/// Types ASCII letters and digits by virtual key, so an active IME sees them as keystrokes.
pub fn type_keys(text: &str) {
  for c in text.chars() {
    tap(VIRTUAL_KEY(c.to_ascii_uppercase() as u16));
  }
}

#[derive(Debug, Serialize)]
pub struct FocusInfo {
  pub foreground: isize,
  pub focus: isize,
  pub focus_class: String,
  pub focus_exe: String,
  pub capture: isize,
  pub capture_class: String,
}

/// Which window holds the foreground and the keyboard focus right now.
pub fn focus_info() -> FocusInfo {
  let mut info = GUITHREADINFO {
    cbSize: size_of::<GUITHREADINFO>() as u32,
    ..Default::default()
  };
  let _ = unsafe { GetGUIThreadInfo(0, &mut info) };
  FocusInfo {
    foreground: info.hwndActive.0 as isize,
    focus: info.hwndFocus.0 as isize,
    focus_class: class_name(info.hwndFocus),
    focus_exe: exe_of_window(info.hwndFocus),
    capture: info.hwndCapture.0 as isize,
    capture_class: class_name(info.hwndCapture),
  }
}

#[derive(Debug, Serialize)]
pub struct PointElement {
  pub name: String,
  pub exe: String,
  pub rect: [i32; 4],
}

/// The UI Automation element at a screen point, with the process that owns it. Microsoft Pinyin
/// draws its candidate list inside a full-screen `TextInputHost.exe` window, so window rectangles
/// say nothing about where the list is; the element under a point does.
pub fn element_at((x, y): (i32, i32)) -> Option<PointElement> {
  unsafe {
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    let automation: IUIAutomation =
      CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
    let element = automation.ElementFromPoint(POINT { x, y }).ok()?;
    let pid = element.CurrentProcessId().ok()? as u32;
    let rect = element.CurrentBoundingRectangle().ok()?;
    let exe = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
      .ok()
      .map(|process| {
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let name = QueryFullProcessImageNameW(
          process,
          PROCESS_NAME_WIN32,
          PWSTR(buf.as_mut_ptr()),
          &mut size,
        )
        .map(|_| String::from_utf16_lossy(&buf[..size as usize]))
        .unwrap_or_default();
        let _ = CloseHandle(process);
        name.rsplit('\\').next().unwrap_or_default().to_owned()
      })
      .unwrap_or_default();
    Some(PointElement {
      name: element
        .CurrentName()
        .map(|n| n.to_string())
        .unwrap_or_default(),
      exe,
      rect: [rect.left, rect.top, rect.right, rect.bottom],
    })
  }
}

#[derive(Debug, Serialize)]
pub struct WindowAt {
  pub root: isize,
  pub class: String,
  pub exe: String,
}

/// The top-level window a click at this screen point would go to (a query, not a click).
pub fn window_at(point: (i32, i32)) -> WindowAt {
  let root = unsafe {
    GetAncestor(
      WindowFromPoint(POINT {
        x: point.0,
        y: point.1,
      }),
      GA_ROOT,
    )
  };
  WindowAt {
    root: root.0 as isize,
    class: class_name(root),
    exe: exe_of_window(root),
  }
}

#[derive(Debug, Serialize)]
pub struct TopLevel {
  pub hwnd: isize,
  pub owner: isize,
  pub class: String,
  pub exe: String,
  pub rect: [i32; 4],
  pub cloaked: u32,
  pub ex_style: String,
}

/// Visible top-level windows owned by this process or the WebView2 processes it spawned.
pub fn own_top_level_windows() -> Vec<TopLevel> {
  unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let all = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
    if unsafe { IsWindowVisible(hwnd) }.as_bool() {
      all.push(hwnd);
    }
    BOOL::from(true)
  }
  let mut pids = descendants(unsafe { GetCurrentProcessId() }, "msedgewebview2.exe");
  pids.push(unsafe { GetCurrentProcessId() });
  let mut all: Vec<HWND> = Vec::new();
  let _ = unsafe { EnumWindows(Some(collect), LPARAM(&mut all as *mut Vec<HWND> as isize)) };
  all
    .into_iter()
    .filter(|&hwnd| {
      let mut pid = 0u32;
      unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
      pids.contains(&pid)
    })
    .map(|hwnd| {
      let mut rect = RECT::default();
      let _ = unsafe { GetWindowRect(hwnd, &mut rect) };
      TopLevel {
        hwnd: hwnd.0 as isize,
        owner: unsafe { GetWindow(hwnd, GW_OWNER) }.map_or(0, |owner| owner.0 as isize),
        class: class_name(hwnd),
        exe: exe_of_window(hwnd),
        rect: [rect.left, rect.top, rect.right, rect.bottom],
        cloaked: cloaked(hwnd),
        ex_style: format!("{:#010x}", unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) }
          as u32),
      }
    })
    .collect()
}

/// Class names of `parent`'s descendant windows that carry an OLE drop target
/// (`RegisterDragDrop` stores it in the `OleDropTargetInterface` window property).
pub fn drop_targets(parent: HWND) -> Vec<String> {
  unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let found = unsafe { &mut *(lparam.0 as *mut Vec<String>) };
    if !unsafe { GetPropW(hwnd, w!("OleDropTargetInterface")) }.is_invalid() {
      found.push(class_name(hwnd));
    }
    BOOL::from(true)
  }
  let mut found: Vec<String> = Vec::new();
  let _ = unsafe {
    EnumChildWindows(
      Some(parent),
      Some(collect),
      LPARAM(&mut found as *mut Vec<String> as isize),
    )
  };
  found
}

/// Class names of `parent`'s descendant windows. A composition-hosted webview has no
/// `WRY_WEBVIEW` container window, so this tells whether wry took the composition path.
pub fn child_classes(parent: HWND) -> Vec<String> {
  unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let classes = unsafe { &mut *(lparam.0 as *mut Vec<String>) };
    classes.push(class_name(hwnd));
    BOOL::from(true)
  }
  let mut classes: Vec<String> = Vec::new();
  let _ = unsafe {
    EnumChildWindows(
      Some(parent),
      Some(collect),
      LPARAM(&mut classes as *mut Vec<String> as isize),
    )
  };
  classes
}

fn class_name(hwnd: HWND) -> String {
  let mut buf = [0u16; 256];
  let len = unsafe { GetClassNameW(hwnd, &mut buf) }.max(0) as usize;
  String::from_utf16_lossy(&buf[..len])
}

fn exe_of_window(hwnd: HWND) -> String {
  let mut pid = 0u32;
  unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
  let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
    return String::new();
  };
  let mut buf = [0u16; 1024];
  let mut size = buf.len() as u32;
  let name = unsafe {
    QueryFullProcessImageNameW(
      process,
      PROCESS_NAME_WIN32,
      PWSTR(buf.as_mut_ptr()),
      &mut size,
    )
  }
  .map(|_| String::from_utf16_lossy(&buf[..size as usize]))
  .unwrap_or_default();
  let _ = unsafe { CloseHandle(process) };
  name.rsplit('\\').next().unwrap_or_default().to_owned()
}
