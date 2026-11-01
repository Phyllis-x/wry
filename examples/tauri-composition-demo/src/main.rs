//! Verifies wry PR #1762 (WebView2 composition hosting), ported to wry 0.57, under Tauri 2.12.
//!
//! The menu window is created on its own, its DirectComposition target is registered with wry,
//! and only then is the webview added (PinPaper ADR-02). The scenarios run on a background thread,
//! drive the window with real input (`SendInput`) and read what the page reports over IPC.
//! Every result is a JSON line on stdout; the exit code is the number of failed checks.
//!
//! Usage: `tauri-composition-demo [scenario...]`, where a scenario is one of `cloak`, `input`,
//! `dragout`, `rebuild`, `move`, `ime`, `closevisual`, `dragdrop`, `dup`, `bounds`, `jsclose`,
//! `longcloak` and `hold`; without arguments it runs `ALL_SCENARIOS`, about three minutes. The
//! input scenarios move the real cursor and type, so leave mouse and keyboard alone meanwhile.
//!
//! - `DEMO_NO_INPUT=1` injects no input at all, cursor moves included.
//! - `DEMO_PAUSE_MS=<ms>` holds each IME composition open that long, for screenshots.
//! - `DEMO_HOLD_MS=<ms>` is how long `hold` keeps the window up (default 20 s).
//! - `DEMO_BATCHES` and `DEMO_REBUILDS` size the `rebuild` scenario (default 3 × 20).
//! - `DEMO_WINDOWED=1` skips the registration, so wry builds ordinary windowed webviews.

mod composition;
mod win;

use std::{
  cell::RefCell,
  error::Error,
  sync::{Condvar, Mutex, OnceLock, mpsc},
  thread,
  time::{Duration, Instant},
};

use serde_json::{Value, json};
use tauri::{
  AppHandle, LogicalPosition, Manager, RunEvent, WebviewUrl, webview::WebviewBuilder,
  window::WindowBuilder,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
  VK_BACK, VK_ESCAPE, VK_RETURN, VK_SHIFT, VK_SPACE,
};

use crate::composition::CompositionTree;

const MENU: &str = "menu";
const PALETTE: [(u8, u8, u8); 4] = [
  (0x2e, 0x7d, 0x32),
  (0x15, 0x65, 0xc0),
  (0x6a, 0x1b, 0x9a),
  (0xc6, 0x28, 0x28),
];
/// Input and IME run again after the rebuilds, the cloaks and a move, as in real use.
const ALL_SCENARIOS: [&str; 13] = [
  "cloak",
  "input",
  "dragout",
  "rebuild",
  "move",
  "input",
  "ime",
  "closevisual",
  "dragdrop",
  "dup",
  "bounds",
  "jsclose",
  "longcloak",
];
const CLOAK_CYCLES: u32 = 100;
const REBUILDS: u32 = 20;

thread_local! {
  // DirectComposition objects are not `Send`; they live on the main (UI, STA) thread.
  static TREE: RefCell<Option<CompositionTree>> = const { RefCell::new(None) };
}

fn start() -> Instant {
  static START: OnceLock<Instant> = OnceLock::new();
  *START.get_or_init(Instant::now)
}

fn log(src: &str, value: Value) {
  println!(
    "{}",
    json!({ "t": start().elapsed().as_millis() as u64, "src": src, "v": value })
  );
}

#[derive(Clone, Debug)]
struct Report {
  kind: String,
  generation: u32,
  data: Value,
  at: Instant,
}

struct Bus {
  events: Mutex<Vec<Report>>,
  changed: Condvar,
}

fn bus() -> &'static Bus {
  static BUS: OnceLock<Bus> = OnceLock::new();
  BUS.get_or_init(|| Bus {
    events: Mutex::new(Vec::new()),
    changed: Condvar::new(),
  })
}

impl Bus {
  fn mark(&self) -> usize {
    self.events.lock().unwrap().len()
  }

  fn wait(
    &self,
    since: usize,
    timeout: Duration,
    pred: impl Fn(&Report) -> bool,
  ) -> Option<Report> {
    let deadline = Instant::now() + timeout;
    let mut events = self.events.lock().unwrap();
    loop {
      if let Some(found) = events[since..].iter().find(|r| pred(r)) {
        return Some(found.clone());
      }
      let now = Instant::now();
      if now >= deadline {
        return None;
      }
      events = self.changed.wait_timeout(events, deadline - now).unwrap().0;
    }
  }

  fn since(&self, since: usize) -> Vec<Report> {
    self.events.lock().unwrap()[since..].to_vec()
  }
}

#[tauri::command]
fn report(kind: String, generation: u32, data: Value) {
  log(
    "page",
    json!({ "kind": kind, "generation": generation, "data": data }),
  );
  let bus = bus();
  bus.events.lock().unwrap().push(Report {
    kind,
    generation,
    data,
    at: Instant::now(),
  });
  bus.changed.notify_all();
}

fn main() {
  start();
  let mut scenarios: Vec<String> = std::env::args().skip(1).collect();
  if scenarios.is_empty() {
    scenarios = ALL_SCENARIOS.iter().map(|s| s.to_string()).collect();
  }

  let app = tauri::Builder::default()
    .invoke_handler(tauri::generate_handler![report])
    .setup(move |app| {
      let handle = app.handle().clone();
      let hwnd = create_menu_window(&handle)?;
      add_menu_webview(&handle, 0, false)?;
      thread::spawn(move || run_scenarios(handle, hwnd, scenarios));
      Ok(())
    })
    .build(tauri::generate_context!())
    .expect("failed to build the tauri app");

  // Keep running if the menu window disappears, so the scenarios can report it.
  app.run(|_, event| {
    if let RunEvent::ExitRequested {
      code: None, api, ..
    } = event
    {
      api.prevent_exit();
    }
  });
}

/// Creates the menu window and its composition tree; no webview yet.
fn create_menu_window(app: &AppHandle) -> Result<isize, Box<dyn Error>> {
  let window = WindowBuilder::new(app, MENU)
    .title("PinPaper composition demo")
    .inner_size(960.0, 640.0)
    .center()
    .decorations(false)
    .transparent(true)
    .no_redirection_bitmap(true)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .shadow(false)
    .build()?;
  let hwnd = window.hwnd()?;
  win::apply_menu_styles(hwnd)?;
  let size = window.inner_size()?;
  let tree = CompositionTree::new(hwnd, size.width as f32, size.height as f32)?;
  TREE.with(|t| *t.borrow_mut() = Some(tree));
  log(
    "demo",
    json!({
      "event": "menu-window",
      "tauri": tauri::VERSION,
      "webview2": wry::webview_version().ok(),
      "scale": window.scale_factor()?,
      "size": [size.width, size.height],
      "styles": win::styles(hwnd),
    }),
  );
  Ok(hwnd.0 as isize)
}

/// Registers the composition target, then adds webview `generation` to the menu window.
/// Must run on the main thread: wry's registry is per UI thread.
fn add_menu_webview(
  app: &AppHandle,
  generation: u32,
  fresh_visual: bool,
) -> Result<(), Box<dyn Error>> {
  add_webview(app, generation, fresh_visual, false)
}

/// `drag_drop` keeps Tauri's default drag-and-drop handler, which the menu window turns off.
fn add_webview(
  app: &AppHandle,
  generation: u32,
  fresh_visual: bool,
  drag_drop: bool,
) -> Result<(), Box<dyn Error>> {
  let window = app.get_window(MENU).ok_or("the menu window is gone")?;
  let hwnd = window.hwnd()?;
  if !windowed() {
    TREE.with(|t| -> windows::core::Result<()> {
      let mut tree = t.borrow_mut();
      let tree = tree
        .as_mut()
        .expect("composition tree is created with the window");
      if fresh_visual {
        tree.replace_webview_visual()?;
      }
      tree.register(hwnd)
    })?;
  }
  let mut builder = WebviewBuilder::new(label(generation), WebviewUrl::App("index.html".into()))
    .initialization_script(format!("window.__GEN = {generation};"))
    .transparent(true);
  if !drag_drop {
    builder = builder.disable_drag_drop_handler();
  }
  window.add_child(
    builder,
    LogicalPosition::new(0.0, 0.0),
    window.inner_size()?,
  )?;
  TREE.with(|t| t.borrow().as_ref().map(CompositionTree::commit).transpose())?;
  log(
    "demo",
    json!({ "event": "webview-added", "generation": generation, "fresh_visual": fresh_visual, "host_children": win::child_classes(hwnd) }),
  );
  Ok(())
}

fn label(generation: u32) -> String {
  format!("menu-{generation}")
}

/// Runs `f` on the main thread and waits for its result.
fn on_main<T: Send + 'static>(
  app: &AppHandle,
  f: impl FnOnce(&AppHandle) -> T + Send + 'static,
) -> T {
  let (tx, rx) = mpsc::channel();
  let handle = app.clone();
  app
    .run_on_main_thread(move || {
      let _ = tx.send(f(&handle));
    })
    .expect("the event loop is gone");
  rx.recv().expect("the main thread dropped the task")
}

struct Ctx {
  app: AppHandle,
  hwnd: isize,
  generation: u32,
  ready: Value,
  failures: Vec<String>,
}

impl Ctx {
  fn check(&mut self, name: &str, pass: bool, detail: Value) -> bool {
    log(
      "check",
      json!({ "name": name, "pass": pass, "detail": detail }),
    );
    if !pass {
      self.failures.push(name.to_owned());
    }
    pass
  }

  fn hwnd(&self) -> windows::Win32::Foundation::HWND {
    win::hwnd(self.hwnd)
  }

  fn eval(&self, js: &str) {
    match self.app.get_webview(&label(self.generation)) {
      Some(webview) => {
        if let Err(error) = webview.eval(js) {
          log(
            "demo",
            json!({ "event": "eval-failed", "js": js, "error": error.to_string() }),
          );
        }
      }
      None => log(
        "demo",
        json!({ "event": "eval-no-webview", "generation": self.generation }),
      ),
    }
  }

  /// Screen coordinates of a point given in CSS pixels of the page.
  fn css_to_screen(&self, x: f64, y: f64) -> (i32, i32) {
    let dpr = self.ready["dpr"].as_f64().unwrap_or(1.0);
    win::client_to_screen(
      self.hwnd(),
      (x * dpr).round() as i32,
      (y * dpr).round() as i32,
    )
  }

  fn rect_center(&self, rect: &str) -> (i32, i32) {
    let r = &self.ready[rect];
    let x = (r["left"].as_f64().unwrap() + r["right"].as_f64().unwrap()) / 2.0;
    let y = (r["top"].as_f64().unwrap() + r["bottom"].as_f64().unwrap()) / 2.0;
    self.css_to_screen(x, y)
  }

  /// A spot that only ever shows the page background.
  fn background_point(&self) -> (i32, i32) {
    self.css_to_screen(880.0, 600.0)
  }

  fn expected_color(&self) -> (u8, u8, u8) {
    PALETTE[self.generation as usize % PALETTE.len()]
  }

  fn wait_ready(&mut self, generation: u32, since: usize) -> bool {
    match bus().wait(since, Duration::from_secs(15), |r| {
      r.kind == "ready" && r.generation == generation
    }) {
      Some(ready) => {
        self.ready = ready.data;
        true
      }
      None => false,
    }
  }

  /// Polls the composed desktop until the page background shows at the sample point.
  fn wait_page_visible(&self, timeout: Duration) -> Option<Duration> {
    let started = Instant::now();
    let expected = self.expected_color();
    let point = self.background_point();
    while started.elapsed() < timeout {
      if close(win::screen_pixel(point), expected) {
        return Some(started.elapsed());
      }
      thread::sleep(Duration::from_millis(2));
    }
    None
  }

  /// Asks the page for a fresh `requestAnimationFrame` callback.
  fn probe(&self, id: u32, timeout: Duration) -> Option<Report> {
    let mark = bus().mark();
    self.eval(&format!("__probe({id})"));
    bus().wait(mark, timeout, |r| r.kind == "probe" && r.data["id"] == id)
  }
}

fn close(a: (u8, u8, u8), b: (u8, u8, u8)) -> bool {
  a.0.abs_diff(b.0) <= 8 && a.1.abs_diff(b.1) <= 8 && a.2.abs_diff(b.2) <= 8
}

fn run_scenarios(app: AppHandle, hwnd: isize, scenarios: Vec<String>) {
  let mut ctx = Ctx {
    app: app.clone(),
    hwnd,
    generation: 0,
    ready: Value::Null,
    failures: Vec::new(),
  };
  win::guard_input_to(ctx.hwnd());
  if std::env::var_os("DEMO_NO_INPUT").is_some() {
    win::disable_input();
  }
  let ready = ctx.wait_ready(0, 0);
  ctx.check("page ready", ready, json!({ "ready": ctx.ready }));
  let composition = !win::child_classes(ctx.hwnd())
    .iter()
    .any(|c| c == "WRY_WEBVIEW");
  ctx.check(
    "webview is composition hosted (no WRY_WEBVIEW container)",
    composition,
    json!({ "host_children": win::child_classes(ctx.hwnd()) }),
  );
  let visible = ctx.wait_page_visible(Duration::from_secs(5));
  ctx.check(
    "page visible on screen",
    visible.is_some(),
    json!({ "pixel": win::screen_pixel(ctx.background_point()), "expected": ctx.expected_color() }),
  );

  if ready {
    for scenario in &scenarios {
      log("demo", json!({ "event": "scenario", "name": scenario }));
      match scenario.as_str() {
        "cloak" => scenario_cloak(&mut ctx),
        "longcloak" => scenario_long_cloak(&mut ctx),
        "input" => scenario_input(&mut ctx),
        "dragout" => scenario_drag_out(&mut ctx),
        "ime" => scenario_ime(&mut ctx),
        "rebuild" => scenario_rebuild(&mut ctx),
        "jsclose" => scenario_jsclose(&mut ctx),
        "dup" => scenario_duplicate(&mut ctx),
        "bounds" => scenario_bounds(&mut ctx),
        "closevisual" => scenario_close_visual(&mut ctx),
        "dragdrop" => scenario_drag_drop(&mut ctx),
        "move" => scenario_move(&mut ctx),
        "hold" => thread::sleep(Duration::from_millis(
          std::env::var("DEMO_HOLD_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20_000),
        )),
        other => log(
          "demo",
          json!({ "event": "unknown-scenario", "name": other }),
        ),
      }
      if !win::is_window(ctx.hwnd()) {
        log("demo", json!({ "event": "menu-window-gone" }));
        break;
      }
    }
  }

  log(
    "summary",
    json!({ "failures": ctx.failures, "input_blocked_by_guard": win::input_blocked() }),
  );
  app.exit(ctx.failures.len() as i32);
}

fn set_cloak(ctx: &Ctx, cloak: bool) {
  let hwnd = ctx.hwnd;
  on_main(&ctx.app, move |_| win::set_cloak(win::hwnd(hwnd), cloak)).expect("DWMWA_CLOAK");
}

/// Cloaks and uncloaks the menu window `CLOAK_CYCLES` times, twice, and compares handle counts.
fn scenario_cloak(ctx: &mut Ctx) {
  for _ in 0..5 {
    set_cloak(ctx, true);
    thread::sleep(Duration::from_millis(30));
    set_cloak(ctx, false);
    ctx.wait_page_visible(Duration::from_secs(2));
  }
  thread::sleep(Duration::from_millis(500));
  let mut snapshots = vec![win::snapshot()];
  let mut id = 0;
  for batch in 0..2 {
    let mut visible_ms = Vec::new();
    let mut probe_ms = Vec::new();
    let mut failures = Vec::new();
    let mut hidden_while_cloaked = 0;
    let since = bus().mark();
    for cycle in 0..CLOAK_CYCLES {
      set_cloak(ctx, true);
      let cloaked = win::cloaked(ctx.hwnd());
      thread::sleep(Duration::from_millis(40));
      if !close(
        win::screen_pixel(ctx.background_point()),
        ctx.expected_color(),
      ) {
        hidden_while_cloaked += 1;
      }
      // Both delays count from the uncloak call: pixel on screen, and the first
      // `requestAnimationFrame` callback the page reports after it.
      id += 1;
      let mark = bus().mark();
      let uncloaked_at = Instant::now();
      set_cloak(ctx, false);
      ctx.eval(&format!("__probe({id})"));
      let visible = ctx
        .wait_page_visible(Duration::from_secs(2))
        .map(|_| uncloaked_at.elapsed());
      let probe = bus().wait(mark, Duration::from_secs(2), |r| {
        r.kind == "probe" && r.data["id"] == id
      });
      match (visible, &probe) {
        (Some(visible), Some(probe)) if cloaked != 0 => {
          visible_ms.push(visible.as_secs_f64() * 1000.0);
          probe_ms.push(probe.at.duration_since(uncloaked_at).as_secs_f64() * 1000.0);
        }
        _ => failures.push(json!({
          "cycle": cycle,
          "cloaked": cloaked,
          "visible": visible.map(|d| d.as_millis() as u64),
          "probe": probe.map(|p| p.data),
        })),
      }
    }
    thread::sleep(Duration::from_millis(1000));
    snapshots.push(win::snapshot());
    let visibility: Vec<Value> = bus()
      .since(since)
      .into_iter()
      .filter(|r| r.kind == "visibility")
      .map(|r| r.data["state"].clone())
      .collect();
    ctx.check(
      &format!(
        "cloak batch {}: {CLOAK_CYCLES} cycles render correctly",
        batch + 1
      ),
      failures.is_empty() && hidden_while_cloaked == CLOAK_CYCLES,
      json!({
        "failures": failures,
        "hidden_while_cloaked": hidden_while_cloaked,
        "uncloak_to_pixel_ms": stats(&visible_ms),
        "uncloak_to_raf_ms": stats(&probe_ms),
        "visibility_changes": visibility,
      }),
    );
  }
  let delta = |i: usize| {
    json!({
      "demo_handles": snapshots[i + 1].demo.handles as i64 - snapshots[i].demo.handles as i64,
      "demo_gdi": snapshots[i + 1].demo.gdi as i64 - snapshots[i].demo.gdi as i64,
      "demo_user": snapshots[i + 1].demo.user as i64 - snapshots[i].demo.user as i64,
      "webview2_handles": snapshots[i + 1].webview2.handles as i64 - snapshots[i].webview2.handles as i64,
      "webview2_gdi": snapshots[i + 1].webview2.gdi as i64 - snapshots[i].webview2.gdi as i64,
      "webview2_user": snapshots[i + 1].webview2.user as i64 - snapshots[i].webview2.user as i64,
    })
  };
  let second = delta(1);
  let no_leak = second["demo_handles"].as_i64().unwrap() <= 10
    && second["demo_gdi"].as_i64().unwrap() <= 2
    && second["demo_user"].as_i64().unwrap() <= 2
    && second["webview2_handles"].as_i64().unwrap() <= 50
    && second["webview2_gdi"].as_i64().unwrap() <= 2
    && second["webview2_user"].as_i64().unwrap() <= 2;
  ctx.check(
    "cloak: no handle growth between batches",
    no_leak,
    json!({ "snapshots": snapshots, "batch1_delta": delta(0), "batch2_delta": second }),
  );
}

/// Data only (PinPaper ARCHITECTURE 4.5.1 asks whether Chromium throttles a cloaked window):
/// stays cloaked for 10 s, probing `requestAnimationFrame` meanwhile, then times the way back.
fn scenario_long_cloak(ctx: &mut Ctx) {
  let since = bus().mark();
  set_cloak(ctx, true);
  let mut probes_while_cloaked = Vec::new();
  for second in 1..=10u32 {
    thread::sleep(Duration::from_millis(1000));
    let probe = ctx.probe(20_000 + second, Duration::from_millis(500));
    probes_while_cloaked.push(json!({ "second": second, "raf": probe.map(|p| p.data) }));
  }
  let mark = bus().mark();
  let uncloaked_at = Instant::now();
  set_cloak(ctx, false);
  ctx.eval("__probe(20100)");
  let visible = ctx
    .wait_page_visible(Duration::from_secs(5))
    .map(|_| uncloaked_at.elapsed());
  let probe = bus().wait(mark, Duration::from_secs(5), |r| {
    r.kind == "probe" && r.data["id"] == 20_100
  });
  let raf_after = probe.map(|p| p.at.duration_since(uncloaked_at));
  let visibility: Vec<Value> = bus()
    .since(since)
    .into_iter()
    .filter(|r| r.kind == "visibility")
    .map(|r| r.data["state"].clone())
    .collect();
  log(
    "info",
    json!({ "name": "long cloak (10 s)", "probes_while_cloaked": probes_while_cloaked,
            "visibility_changes": visibility,
            "uncloak_to_pixel_ms": visible.map(|d| d.as_secs_f64() * 1000.0),
            "uncloak_to_raf_ms": raf_after.map(|d| d.as_secs_f64() * 1000.0) }),
  );
}

fn stats(values: &[f64]) -> Value {
  if values.is_empty() {
    return Value::Null;
  }
  let mut sorted = values.to_vec();
  sorted.sort_by(f64::total_cmp);
  let pick = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
  json!({ "n": sorted.len(), "p50": pick(0.5), "p95": pick(0.95), "max": sorted[sorted.len() - 1] })
}

/// Input is only injected while the page is what the screen shows at the menu window.
fn on_screen(ctx: &mut Ctx, scenario: &str) -> bool {
  if win::input_blocked() {
    log(
      "demo",
      json!({ "event": "skipped", "scenario": scenario, "reason": "input disabled" }),
    );
    return false;
  }
  let visible = ctx.wait_page_visible(Duration::from_secs(2)).is_some();
  ctx.check(
    &format!("{scenario}: menu window is on screen before injecting input"),
    visible,
    json!({ "pixel": win::screen_pixel(ctx.background_point()), "expected": ctx.expected_color() }),
  )
}

fn wait_kind(generation: u32, since: usize, kind: &str, timeout: Duration) -> Option<Report> {
  bus().wait(since, timeout, |r| {
    r.generation == generation && r.kind == kind
  })
}

/// Mouse click, hover, wheel and plain keys reach the page.
fn scenario_input(ctx: &mut Ctx) {
  if !on_screen(ctx, "input") {
    return;
  }
  let generation = ctx.generation;
  let short = Duration::from_millis(1500);
  let background = ctx.background_point();
  let target = ctx.rect_center("target");

  let mark = bus().mark();
  win::move_to(background);
  win::glide(background, target);
  let enter = wait_kind(generation, mark, "pointerenter", short);
  ctx.eval("__hoverState()");
  let hover = wait_kind(generation, mark, "hover-state", short);
  ctx.check(
    "hover: pointerenter and :hover",
    enter.is_some() && hover.as_ref().is_some_and(|h| h.data["hover"] == true),
    json!({ "pointerenter": enter.map(|r| r.data), "hover": hover.map(|r| r.data) }),
  );

  let mark = bus().mark();
  win::left_click();
  let down = wait_kind(generation, mark, "mousedown", short);
  let up = wait_kind(generation, mark, "mouseup", short);
  let click = wait_kind(generation, mark, "click", short);
  ctx.check(
    "click: mousedown, mouseup, click on the target",
    [&down, &up, &click]
      .iter()
      .all(|r| r.as_ref().is_some_and(|r| r.data["target"] == "target" && r.data["button"] == 0)),
    json!({ "mousedown": down.map(|r| r.data), "mouseup": up.map(|r| r.data), "click": click.map(|r| r.data) }),
  );

  let mark = bus().mark();
  win::wheel(-120);
  let vertical = bus().wait(mark, short, |r| {
    r.generation == generation
      && r.kind == "wheel"
      && r.data["deltaY"].as_f64().unwrap_or(0.0) > 0.0
  });
  let mark = bus().mark();
  win::hwheel(120);
  let horizontal = bus().wait(mark, short, |r| {
    r.generation == generation
      && r.kind == "wheel"
      && r.data["deltaX"].as_f64().unwrap_or(0.0) > 0.0
  });
  ctx.check(
    "wheel: vertical and horizontal",
    vertical.is_some() && horizontal.is_some(),
    json!({ "vertical": vertical.map(|r| r.data), "horizontal": horizontal.map(|r| r.data) }),
  );

  let mark = bus().mark();
  win::glide(target, background);
  let leave_target = wait_kind(generation, mark, "pointerleave", short);
  let (left, top) = win::client_to_screen(ctx.hwnd(), 0, 0);
  win::glide(background, (left + 200, top - 40));
  let leave_window = wait_kind(generation, mark, "document-mouseleave", short);
  ctx.check(
    "leave: target and window",
    leave_target.is_some() && leave_window.is_some(),
    json!({ "pointerleave": leave_target.is_some(), "document_mouseleave": leave_window.is_some() }),
  );

  let text = ctx.rect_center("text");
  let mark = bus().mark();
  win::glide((left + 200, top - 40), text);
  win::left_click();
  let focused = wait_kind(generation, mark, "text-focus", short);
  log(
    "demo",
    json!({ "event": "focus", "focus": win::focus_info() }),
  );
  win::type_keys("123");
  win::tap(VK_BACK);
  win::tap(VK_RETURN);
  let typed = bus().wait(mark, short, |r| {
    r.generation == generation && r.kind == "input" && r.data["value"] == "12\n"
  });
  let keys: Vec<Value> = bus()
    .since(mark)
    .into_iter()
    .filter(|r| r.kind == "keydown")
    .map(|r| r.data["key"].clone())
    .collect();
  ctx.check(
    "keyboard: digits, Backspace, Enter",
    focused.is_some() && typed.is_some(),
    json!({ "text_focus": focused.is_some(), "keydown": keys, "value": typed.map(|r| r.data["value"].clone()) }),
  );

  // Letters: Microsoft Pinyin starts in Chinese mode; Shift switches it to English mode.
  let mut letters = None;
  for attempt in 0..2 {
    let mark = bus().mark();
    win::tap(VK_SHIFT);
    win::type_keys("abc");
    if wait_kind(
      generation,
      mark,
      "compositionstart",
      Duration::from_millis(400),
    )
    .is_some()
    {
      win::tap(VK_ESCAPE);
      log(
        "demo",
        json!({ "event": "letters-composed", "attempt": attempt }),
      );
      continue;
    }
    letters = bus().wait(mark, short, |r| {
      r.generation == generation
        && r.kind == "input"
        && r.data["value"].as_str().is_some_and(|v| v.ends_with("abc"))
    });
    break;
  }
  win::tap(VK_SHIFT);
  ctx.check(
    "keyboard: letters in English mode",
    letters.is_some(),
    json!({ "value": letters.map(|r| r.data["value"].clone()) }),
  );
}

/// WebView2Feedback #5712 / #5695: in visual hosting, dragging out of the window with the left
/// button held, a few times over, left the page deaf to mouse and keyboard on runtimes 152+.
fn scenario_drag_out(ctx: &mut Ctx) {
  if !on_screen(ctx, "dragout") {
    return;
  }
  let generation = ctx.generation;
  let short = Duration::from_millis(1500);
  let target = ctx.rect_center("target");
  let (left, top) = win::client_to_screen(ctx.hwnd(), 0, 0);
  let outside = (left + 200, top - 60);
  let mut releases = Vec::new();
  for attempt in 0..5 {
    let mark = bus().mark();
    win::move_to(target);
    win::left_down();
    let pressed = win::focus_info();
    win::glide(target, outside);
    let outside_window = win::focus_info();
    win::left_up();
    log(
      "demo",
      json!({ "event": "drag-out", "attempt": attempt, "host": ctx.hwnd,
              "after_press": pressed, "outside_before_release": outside_window }),
    );
    // Mouse capture keeps the release with the page even outside the window.
    let up = wait_kind(generation, mark, "mouseup", Duration::from_millis(800));
    releases.push(up.map(|r| r.data["y"].clone()));
    thread::sleep(Duration::from_millis(200));
  }
  let html_drags = bus()
    .since(0)
    .iter()
    .filter(|r| r.kind == "dragstart")
    .count();
  ctx.check(
    "dragout: the page gets the release outside the window (capture)",
    releases
      .iter()
      .all(|y| y.as_ref().and_then(Value::as_f64).is_some_and(|y| y < 0.0)),
    json!({ "mouseup_client_y": releases, "html_dragstarts": html_drags }),
  );

  let mark = bus().mark();
  win::glide(outside, target);
  let enter = wait_kind(generation, mark, "pointerenter", short);
  win::left_click();
  let click = bus().wait(mark, short, |r| {
    r.generation == generation && r.kind == "click" && r.data["target"] == "target"
  });
  let text = ctx.rect_center("text");
  win::glide(target, text);
  win::left_click();
  thread::sleep(Duration::from_millis(100));
  let typed_mark = bus().mark();
  win::type_keys("456");
  let typed = bus().wait(typed_mark, short, |r| {
    r.generation == generation && r.kind == "input" && r.data["value"] == "456"
  });
  ctx.eval("__clearText()");
  ctx.check(
    "dragout: hover, click and keys still reach the page after 5 drag-outs",
    enter.is_some() && click.is_some() && typed.is_some(),
    json!({ "pointerenter": enter.is_some(), "click": click.is_some(), "typed": typed.map(|r| r.data["value"].clone()) }),
  );
}

fn is_cjk(text: &str) -> bool {
  !text.is_empty() && text.chars().all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

/// Microsoft Pinyin: composition stays whole and the candidate window sits by the caret.
fn scenario_ime(ctx: &mut Ctx) {
  if !on_screen(ctx, "ime") {
    return;
  }
  let generation = ctx.generation;
  let pause = std::env::var("DEMO_PAUSE_MS")
    .ok()
    .and_then(|v| v.parse().ok())
    .map(Duration::from_millis);
  ctx.eval("__clearText()");
  let text = ctx.rect_center("text");
  win::move_to(text);
  win::left_click();
  thread::sleep(Duration::from_millis(200));

  let mut committed = String::new();
  for (pinyin, expected) in [("nihao", "你好"), ("zhongwenshurufa", "中文输入法")] {
    let mut composing = None;
    let mut mark = bus().mark();
    for _ in 0..2 {
      mark = bus().mark();
      win::type_keys(pinyin);
      composing = wait_kind(
        generation,
        mark,
        "compositionupdate",
        Duration::from_millis(800),
      );
      if composing.is_some() {
        break;
      }
      // English mode: undo the letters and switch to Chinese mode.
      for _ in 0..pinyin.len() {
        win::tap(VK_BACK);
      }
      win::tap(VK_SHIFT);
    }
    thread::sleep(Duration::from_millis(500));
    // Composition start (caret x, bottom of its line) on screen, versus the candidate items.
    let start = wait_kind(
      generation,
      mark,
      "compositionstart",
      Duration::from_millis(100),
    );
    let caret = start.as_ref().map(|r| {
      let caret = &r.data["caret"];
      ctx.css_to_screen(
        caret["x"].as_f64().unwrap_or(0.0),
        caret["lineBottom"].as_f64().unwrap_or(0.0),
      )
    });
    // What sits just below the composition start, and far below it: only the first should be
    // the IME's candidate UI (TextInputHost.exe), and its box should start at the caret.
    let below = caret.and_then(|(x, y)| win::element_at((x + 60, y + 20)));
    let far = caret.and_then(|(x, y)| win::element_at((x + 60, y + 400)));
    let ime = |e: &Option<win::PointElement>| {
      e.as_ref()
        .is_some_and(|e| e.exe.eq_ignore_ascii_case("TextInputHost.exe"))
    };
    let offset = caret
      .zip(below.as_ref())
      .map(|((x, y), e)| (e.rect[0] - x, e.rect[1] - y));
    let near_caret = ime(&below)
      && !ime(&far)
      && offset.is_some_and(|(dx, dy)| (-20..=120).contains(&dx) && (-10..=60).contains(&dy));
    log(
      "demo",
      json!({ "event": "ime-composing", "pinyin": pinyin, "caret_screen": caret,
              "below_caret": below, "far_below": far, "focus": win::focus_info() }),
    );
    if let Some(pause) = pause {
      log(
        "demo",
        json!({ "event": "pause", "ms": pause.as_millis() as u64 }),
      );
      thread::sleep(pause);
    }
    win::tap(VK_SPACE);
    let end = wait_kind(generation, mark, "compositionend", Duration::from_secs(2));
    thread::sleep(Duration::from_millis(300));
    let data = end
      .as_ref()
      .and_then(|r| r.data["data"].as_str())
      .unwrap_or_default()
      .to_owned();
    committed.push_str(&data);
    let state_mark = bus().mark();
    ctx.eval("__textState()");
    let value = wait_kind(generation, state_mark, "text-state", Duration::from_secs(1))
      .and_then(|r| r.data["value"].as_str().map(str::to_owned))
      .unwrap_or_default();
    let ends: usize = bus()
      .since(mark)
      .into_iter()
      .filter(|r| r.kind == "compositionend")
      .count();
    ctx.check(
      &format!("ime: '{pinyin}' commits as one word"),
      composing.is_some() && ends == 1 && is_cjk(&data) && value == committed,
      json!({ "compositionupdate": composing.map(|r| r.data["data"].clone()), "compositionend": data,
              "expected": expected, "exact": data == expected, "value": value, "compositionend_count": ends }),
    );
    ctx.check(
      &format!("ime: candidate list next to the composition for '{pinyin}'"),
      near_caret,
      json!({ "caret_screen": caret, "below_caret": below, "far_below": far, "offset_px": offset }),
    );
  }
}

/// Destroys and recreates the webview on the same host window, in three batches of `REBUILDS`,
/// and compares resource counts between batches.
fn scenario_rebuild(ctx: &mut Ctx) {
  let env = |name: &str, default: u32| {
    std::env::var(name)
      .ok()
      .and_then(|v| v.parse().ok())
      .unwrap_or(default)
  };
  let (batches, per_batch) = (env("DEMO_BATCHES", 3), env("DEMO_REBUILDS", REBUILDS));
  let mut snapshots = vec![win::snapshot()];
  let mut iteration = 0;
  for _batch in 0..batches {
    for _ in 0..per_batch {
      iteration += 1;
      if !rebuild_once(ctx, iteration) {
        return;
      }
    }
    thread::sleep(Duration::from_secs(3));
    snapshots.push(win::snapshot());
  }
  let delta = |i: usize| {
    json!({
      "demo_handles": snapshots[i + 1].demo.handles as i64 - snapshots[i].demo.handles as i64,
      "demo_gdi": snapshots[i + 1].demo.gdi as i64 - snapshots[i].demo.gdi as i64,
      "demo_user": snapshots[i + 1].demo.user as i64 - snapshots[i].demo.user as i64,
      "webview2_handles": snapshots[i + 1].webview2.handles as i64 - snapshots[i].webview2.handles as i64,
      "webview2_user": snapshots[i + 1].webview2.user as i64 - snapshots[i].webview2.user as i64,
      "webview2_processes": snapshots[i + 1].webview2_processes as i64 - snapshots[i].webview2_processes as i64,
    })
  };
  if batches < 3 {
    log(
      "info",
      json!({ "name": "rebuild resources", "snapshots": snapshots }),
    );
    return;
  }
  let last = delta(2);
  ctx.check(
    &format!("rebuild: no resource build-up in the third batch of {per_batch}"),
    last["webview2_processes"].as_i64() == Some(0)
      && last["demo_handles"].as_i64().unwrap() <= 20
      && last["demo_gdi"].as_i64().unwrap() <= 2
      && last["demo_user"].as_i64().unwrap() <= 2,
    json!({ "snapshots": snapshots, "batch_deltas": [delta(0), delta(1), last] }),
  );
}

/// Closes the current webview, checks the host survives, and builds the next generation on it.
fn rebuild_once(ctx: &mut Ctx, iteration: u32) -> bool {
  let old = label(ctx.generation);
  let closed = on_main(&ctx.app, move |app| match app.get_webview(&old) {
    Some(webview) => webview.close().map_err(|e| e.to_string()),
    None => Err(format!("no webview {old}")),
  });
  thread::sleep(Duration::from_millis(300));
  let alive = win::is_window(ctx.hwnd());
  if !ctx.check(
    &format!("rebuild {iteration}: host window survives closing the webview"),
    alive && closed.is_ok(),
    json!({ "close": closed, "is_window": alive }),
  ) {
    return false;
  }

  let generation = ctx.generation + 1;
  let fresh_visual = iteration.is_multiple_of(2);
  let mark = bus().mark();
  let added = on_main(&ctx.app, move |app| {
    add_menu_webview(app, generation, fresh_visual).map_err(|e| e.to_string())
  });
  let ready = added.is_ok() && ctx.wait_ready(generation, mark);
  ctx.generation = generation;
  let visible = ready
    .then(|| ctx.wait_page_visible(Duration::from_secs(5)))
    .flatten();
  let input = !win::input_blocked();
  let mark = bus().mark();
  win::move_to(ctx.background_point());
  win::left_click();
  let click = input
    .then(|| wait_kind(generation, mark, "click", Duration::from_millis(1500)))
    .flatten();
  ctx.check(
    &format!("rebuild {iteration}: new webview renders and takes input"),
    ready && (visible.is_some() || windowed()) && (click.is_some() || !input),
    json!({ "add": added, "fresh_visual": fresh_visual, "ready": ready,
            "visible_ms": visible.map(|d| d.as_millis() as u64), "click": click.is_some(),
            "host_children": win::child_classes(ctx.hwnd()) }),
  )
}

/// Baseline mode: skip the registration so wry builds ordinary windowed webviews.
fn windowed() -> bool {
  std::env::var_os("DEMO_WINDOWED").is_some()
}

/// The open sequence of PinPaper ARCHITECTURE 4.5.1: move the window while cloaked, then uncloak.
/// Later scenarios (input, IME) then run at the new position.
fn scenario_move(ctx: &mut Ctx) {
  let hwnd = ctx.hwnd;
  let moved = on_main(&ctx.app, move |_| -> Result<(i32, i32), String> {
    let hwnd = win::hwnd(hwnd);
    let (x, y) = win::window_origin(hwnd);
    let target = (x - 300, y + 120);
    win::set_cloak(hwnd, true).map_err(|e| e.to_string())?;
    win::move_window(hwnd, target).map_err(|e| e.to_string())?;
    win::set_cloak(hwnd, false).map_err(|e| e.to_string())?;
    Ok(target)
  });
  let visible = ctx.wait_page_visible(Duration::from_secs(2));
  let now = win::window_origin(ctx.hwnd());
  ctx.check(
    "move: the page shows at the window's new position",
    moved.as_ref().is_ok_and(|&target| target == now) && visible.is_some(),
    json!({ "moved_to": moved, "window_origin": now,
            "visible_ms": visible.map(|d| d.as_millis() as u64) }),
  );
}

/// With Tauri's default drag-and-drop handler on, wry must not register OLE drop targets on the
/// host's child windows: in composition mode drops belong to the host (PinPaper ARCHITECTURE
/// 4.5.1 has it implement `IDropTarget` itself).
fn scenario_drag_drop(ctx: &mut Ctx) {
  let old = label(ctx.generation);
  let closed = on_main(&ctx.app, move |app| match app.get_webview(&old) {
    Some(webview) => webview.close().map_err(|e| e.to_string()),
    None => Err(format!("no webview {old}")),
  });
  thread::sleep(Duration::from_millis(300));
  let generation = ctx.generation + 1;
  let mark = bus().mark();
  let added = on_main(&ctx.app, move |app| {
    add_webview(app, generation, false, true).map_err(|e| e.to_string())
  });
  let ready = added.is_ok() && ctx.wait_ready(generation, mark);
  ctx.generation = generation;
  let targets = win::drop_targets(ctx.hwnd());
  ctx.check(
    "dragdrop: no OLE drop targets on the host's child windows",
    closed.is_ok() && ready && targets.is_empty(),
    json!({ "close": closed, "add": added, "ready": ready, "drop_targets": targets,
            "host_children": win::child_classes(ctx.hwnd()) }),
  );
}

/// After the webview is closed and the host commits, its last frame must not stay on screen,
/// then the next generation is built on the same visual.
fn scenario_close_visual(ctx: &mut Ctx) {
  let old = label(ctx.generation);
  let closed = on_main(&ctx.app, move |app| match app.get_webview(&old) {
    Some(webview) => webview.close().map_err(|e| e.to_string()),
    None => Err(format!("no webview {old}")),
  });
  thread::sleep(Duration::from_millis(300));
  let committed = on_main(&ctx.app, |_| {
    TREE
      .with(|t| t.borrow().as_ref().map(CompositionTree::commit).transpose())
      .map_err(|e| e.to_string())
  });
  thread::sleep(Duration::from_millis(500));
  let pixel = win::screen_pixel(ctx.background_point());
  let gone = !close(pixel, ctx.expected_color());
  ctx.check(
    "closevisual: the closed webview's last frame leaves the screen after a commit",
    closed.is_ok() && committed.is_ok() && gone,
    json!({ "close": closed, "commit": committed, "pixel": pixel, "old_color": ctx.expected_color() }),
  );
  let generation = ctx.generation + 1;
  let mark = bus().mark();
  let added = on_main(&ctx.app, move |app| {
    add_menu_webview(app, generation, false).map_err(|e| e.to_string())
  });
  let ready = added.is_ok() && ctx.wait_ready(generation, mark);
  ctx.generation = generation;
  let visible = ready
    .then(|| ctx.wait_page_visible(Duration::from_secs(5)))
    .flatten();
  ctx.check(
    "closevisual: a new webview on the same visual shows up",
    visible.is_some(),
    json!({ "add": added, "ready": ready }),
  );
}

/// `Webview::size` reports the size the webview was given, not the host window's.
fn scenario_bounds(ctx: &mut Ctx) {
  let current = label(ctx.generation);
  let result = on_main(&ctx.app, move |app| -> Result<Value, String> {
    let webview = app.get_webview(&current).ok_or("no webview")?;
    let window = app.get_window(MENU).ok_or("no window")?;
    let full = window.inner_size().map_err(|e| e.to_string())?;
    webview
      .set_size(tauri::LogicalSize::new(800.0, 500.0))
      .map_err(|e| e.to_string())?;
    let size = webview.size().map_err(|e| e.to_string())?;
    let position = webview.position().map_err(|e| e.to_string())?;
    webview.set_size(full).map_err(|e| e.to_string())?;
    let restored = webview.size().map_err(|e| e.to_string())?;
    Ok(json!({
      "set": [800.0 * window.scale_factor().unwrap_or(1.0), 500.0 * window.scale_factor().unwrap_or(1.0)],
      "size": [size.width, size.height], "position": [position.x, position.y],
      "restored": [restored.width, restored.height], "full": [full.width, full.height],
    }))
  });
  let pixels = |v: &Value| v.as_f64().map(|f| f.round() as i64);
  let pass = result.as_ref().is_ok_and(|v| {
    pixels(&v["size"][0]) == pixels(&v["set"][0])
      && pixels(&v["size"][1]) == pixels(&v["set"][1])
      && v["restored"] == v["full"]
  });
  ctx.check(
    "bounds: Webview::size follows set_size",
    pass,
    json!({ "result": result }),
  );
}

/// A second composition webview on the same host window is refused; the first keeps working.
fn scenario_duplicate(ctx: &mut Ctx) {
  let extra = ctx.generation + 100;
  let added = on_main(&ctx.app, move |app| {
    add_menu_webview(app, extra, false).map_err(|e| e.to_string())
  });
  if added.is_ok() {
    let extra = label(extra);
    on_main(&ctx.app, move |app| {
      app.get_webview(&extra).map(|webview| webview.close())
    })
    .transpose()
    .ok();
  }
  ctx.check(
    "duplicate: a second composition webview on the same window is refused",
    added
      .as_ref()
      .is_err_and(|e| e.contains("already hosts a composition webview")),
    json!({ "add": added }),
  );
  let probe = ctx.probe(7_777, Duration::from_secs(2));
  let visible = ctx.wait_page_visible(Duration::from_secs(2));
  ctx.check(
    "duplicate: the first webview keeps rendering",
    probe.is_some() && visible.is_some(),
    json!({ "probe": probe.map(|p| p.data), "visible": visible.is_some() }),
  );
}

/// `window.close()` from the page must not take the host window down.
fn scenario_jsclose(ctx: &mut Ctx) {
  ctx.eval("window.close()");
  thread::sleep(Duration::from_millis(1500));
  let alive = win::is_window(ctx.hwnd());
  let probe = alive
    .then(|| ctx.probe(9_999, Duration::from_secs(2)))
    .flatten();
  ctx.check(
    "jsclose: window.close() leaves the host window and webview alive",
    alive && probe.is_some(),
    json!({ "is_window": alive, "probe": probe.map(|p| p.data) }),
  );
}
