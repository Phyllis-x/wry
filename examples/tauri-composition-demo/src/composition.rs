//! The menu window's DirectComposition tree (PinPaper ARCHITECTURE 4.5.1):
//! target -> root visual (clipped to the window) -> animation container -> webview visual.

use windows::{
  Win32::{
    Foundation::HWND,
    Graphics::{
      Direct2D::Common::D2D_RECT_F,
      DirectComposition::{
        DCompositionCreateDevice2, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
      },
    },
  },
  core::{IUnknown, Interface, Result},
};

pub struct CompositionTree {
  device: IDCompositionDevice,
  _target: IDCompositionTarget,
  _root: IDCompositionVisual,
  container: IDCompositionVisual,
  webview_visual: IDCompositionVisual,
}

impl CompositionTree {
  pub fn new(hwnd: HWND, width: f32, height: f32) -> Result<Self> {
    unsafe {
      let device: IDCompositionDevice = DCompositionCreateDevice2(None::<&IUnknown>)?;
      let target = device.CreateTargetForHwnd(hwnd, true)?;
      let root = device.CreateVisual()?;
      let container = device.CreateVisual()?;
      let webview_visual = device.CreateVisual()?;
      root.SetClip2(&D2D_RECT_F {
        left: 0.0,
        top: 0.0,
        right: width,
        bottom: height,
      })?;
      root.AddVisual(&container, false, None::<&IDCompositionVisual>)?;
      container.AddVisual(&webview_visual, false, None::<&IDCompositionVisual>)?;
      target.SetRoot(&root)?;
      device.Commit()?;
      Ok(Self {
        device,
        _target: target,
        _root: root,
        container,
        webview_visual,
      })
    }
  }

  /// Registers the webview visual as the composition target of the next webview built on `hwnd`.
  pub fn register(&self, hwnd: HWND) -> Result<()> {
    wry::register_composition_visual_target(
      hwnd.0 as isize,
      self.webview_visual.cast::<IUnknown>()?,
    );
    Ok(())
  }

  /// WebView2 connects its visual tree to the target while the webview is built; it only shows
  /// up after the host commits its own device.
  pub fn commit(&self) -> Result<()> {
    unsafe { self.device.Commit() }
  }

  /// Swaps a fresh visual in for the webview, for rebuilds that must not reuse the old one.
  pub fn replace_webview_visual(&mut self) -> Result<()> {
    unsafe {
      let visual = self.device.CreateVisual()?;
      self.container.RemoveVisual(&self.webview_visual)?;
      self
        .container
        .AddVisual(&visual, false, None::<&IDCompositionVisual>)?;
      self.device.Commit()?;
      self.webview_visual = visual;
    }
    Ok(())
  }
}
