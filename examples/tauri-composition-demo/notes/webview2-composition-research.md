# WebView2 Visual Hosting（DirectComposition）调研：输入、IME、关闭重建、cloak 与遮挡

调研日期：2026-11-01。只采信一手资料：Microsoft Learn 上的 WebView2 / DirectComposition / DWM / Win32 文档，[MicrosoftEdge/WebView2Feedback](https://github.com/MicrosoftEdge/WebView2Feedback) 与 [MicrosoftEdge/WebView2Samples](https://github.com/MicrosoftEdge/WebView2Samples) 仓库，以及 chromium.googlesource.com 上的 Chromium 源码。

阅读约定：

- Chromium 源码链接固定在 commit [`cc7cf7c7`](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349)（2026-11-01 的 main），WebView2Samples 链接固定在 commit [`8f5ad22c`](https://github.com/MicrosoftEdge/WebView2Samples/tree/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8)，引用的关键行都在该 commit 上核对过。
- WebView2 的 Win32 逐成员参考页顶部都标注 "This reference is no longer being maintained"；新入口 [WebView2 API Reference](https://learn.microsoft.com/en-us/microsoft-edge/webview2/webview2-api-reference) 的 Win32/C++ 一栏只给出一个 "API Reference for WebView2 Win32 C++" 链接。下文引用的仍是这套逐成员参考页，它的版本列表里已包含 1.0.4314.35 等新版本。
- 标“（推断）”的句子是由所引事实推出的结论，原文没有直接这么写；标“（未文档化）”表示一手资料没有说明。
- WebView2 运行时的嵌入层是闭源的。Chromium 源码只能说明上游 Chromium 的行为，不能直接证明 WebView2 怎么做。

## 要点速览

- **键盘和 IME 不需要应用转发。** visual hosting 下应用只转发“空间输入”（鼠标、触摸、笔），WebView2 拿到焦点后由系统直接把键盘消息送给它（[微软成员在 #1054 的回答](https://github.com/MicrosoftEdge/WebView2Feedback/issues/1054)）。
- **IME 候选窗的位置没有文档。** 上游 Chromium 按“视图在屏幕上的位置”计算候选窗坐标；WebView2 能拿到的位置信息只有父 HWND、`Bounds` 和 `NotifyParentWindowPositionChanged`（推断），祖先 visual 上的偏移或变换不会被候选窗、`<select>` 下拉框看到。composition 模式下有多个仍 open 的 IME issue（#2758、#5570）。
- **`Close()` 后复用同一个 visual 没有文档。** 微软示例是先 `put_RootVisualTarget(nullptr)`、拆掉整棵 DComp 树（含 target）并 `Commit`，最后才 `Close()`；重建时换新设备、新 target、新 visual。
- **cloak 宿主窗口大概率不会让页面变成 hidden。** Chromium 原生遮挡跟踪默认开启并会查询 cloak 状态，但 WebView2 是否对宿主启用它没有文档；WebView2Feedback 的分诊结论是宿主最小化/被遮挡时页面不变 hidden，“by design”，页面可见性跟随 `IsVisible`（推断）。
- **命令行开关。** 四个相关开关里只有 `disable-background-timer-throttling` 出现在 WebView2 官方 flags 列表；`put_AdditionalBrowserArguments` 会整体替换，环境变量和注册表里的参数则是追加；多个特性必须写在同一个 `--disable-features=` 里。
- **152+ 运行时有 composition 输入回归。** 拖出窗口后 `SendMouseInput` 仍返回 `S_OK`，页面却不再响应鼠标和键盘（#5712、#5695 仍 open，155.0.4283.62 仍可复现）。

---

## 1. WebView2 visual hosting 基础

### 1a. `RootVisualTarget` 能是什么；示例用哪种 DComp 设备

**可接受的类型。** 三处官方文档的措辞不完全一致：

- 逐成员参考（Win32 与 .NET 相同）："The RootVisualTarget property can be an IDCompositionVisual or a Windows::UI::Composition::ContainerVisual. WebView will connect its visual tree to the provided visual before returning from the property setter. The app needs to commit on its device setting the RootVisualTarget property. The RootVisualTarget property supports being set to nullptr to disconnect the WebView from the app's visual tree."（[ICoreWebView2CompositionController::get_RootVisualTarget](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2compositioncontroller#get_rootvisualtarget)，[.NET RootVisualTarget](https://learn.microsoft.com/en-us/dotnet/api/microsoft.web.webview2.core.corewebview2compositioncontroller.rootvisualtarget)）
- API 总览多列了 `IDCompositionTarget`："WebView2 can connect its composition tree to an IDCompositionVisual, IDCompositionTarget, or `Windows::UI::Composition::ContainerVisual`."（[Overview: Connecting to the visual tree](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/overview-features-apis#connecting-to-the-visual-tree)）
- hosting 概念页写的是 `IDCompositionVisual`、`IDCompositionTarget` 或 `Windows.UI.Composition.Visual`，并说明 "`CreateCoreWebView2CompositionController` does not support WinAppSDK Visuals"（即 `Microsoft.UI.Composition`）（[Windowed vs. Visual hosting](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/windowed-vs-visual-hosting#framework-constraints)）。参考页和示例都用 `ContainerVisual`，以参考页为准。

**示例里三种模式都有。** Visual DComp 把应用自己的 `IDCompositionVisual` 交给 `put_RootVisualTarget`；Target DComp 交的是应用自己实现的 `IDCompositionTarget`（`DCompTargetImpl`）；WinComp 交 `ContainerVisual`（[ViewComponent.cpp L117-L151](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/ViewComponent.cpp#L117-L151)）。`DCompTargetImpl::SetRoot(visual)` 先 `RemoveAllVisuals()`，再把传入的 visual `AddVisual` 到应用的 webview visual 下（[DCompTargetImpl.cpp L19-L31](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/DCompTargetImpl.cpp#L19-L31)，[DCompTargetImpl.h](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/DCompTargetImpl.h#L11-L25)）。可见在 target 模式下，WebView2 会调用应用 target 的 `SetRoot`，把它自己的根 visual 交出来（推断，依据就是这段示例代码）。

**示例的树结构。** `BuildDCompTreeUsingVisual()` 做三件事：`CreateTargetForHwnd(mainWindow, TRUE)`；建一个 visual 作为 target 的根；再建一个 visual `AddVisual` 到根下，这个就是 WebView 的根 visual（[ViewComponent.cpp L902-L922](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/ViewComponent.cpp#L902-L922)）。本项目的 root → animation container → webview visual 是同一结构，只多一层。

**示例的设备：`DCompositionCreateDevice2`，渲染设备传 `nullptr`，请求 `IDCompositionDevice` 接口。** `AppWindow::InitializeWebView()` 先 `m_dcompDevice = nullptr`，再调用 `DCompositionCreateDevice2(nullptr, IID_PPV_ARGS(&m_dcompDevice))`（[AppWindow.cpp L1816-L1831](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/AppWindow.cpp#L1816-L1831)），`m_dcompDevice` 声明为 `wil::com_ptr<IDCompositionDevice>`（[AppWindow.h L326](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/AppWindow.h#L326)）。同文件末尾的 `AppWindow::DCompositionCreateDevice2` 只是动态加载 `dcomp.dll` 的包装（[AppWindow.cpp](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/AppWindow.cpp)）。设备本身不在 `ViewComponent` 里创建，而是从 `AppWindow` 传进去。

`DCompositionCreateDevice2` 文档对这种用法的说明（[DCompositionCreateDevice2](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-dcompositioncreatedevice2)）：

- `renderingDevice` 为 NULL 时设备不能直接创建 surface，`CreateSurface`/`CreateVirtualSurface` 返回 `E_INVALIDARG`，但可以经 surface factory 间接创建。创建 visual 不受影响。
- 以 `__uuidof(IDCompositionDevice)` 请求时，底层对象是 V2 desktop device，可 QI 出新接口，它创建的 visual 也是 V2 对象。示例在 `SetTransform` 中就对这类 visual 用了 4×4 矩阵变换（[ViewComponent.cpp L648-L697](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/ViewComponent.cpp#L648-L697)），这是 `IDCompositionVisual3` 的方法（[IDCompositionVisual3](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nn-dcomp-idcompositionvisual3)）。

**跨设备组合。** 父子 visual 可以来自不同设备："A child visual need not have been created by the same IDCompositionDevice interface as its parent. … changes to a particular visual take effect only when IDCompositionDevice::Commit is called on the device object that created the visual."（[IDCompositionVisual::AddVisual](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositionvisual-addvisual)）（推断）`IDCompositionVisual`/`IDCompositionTarget` 的公开方法里没有取回创建设备的接口，所以 WebView2 挂进来的 visual 应由它自己的设备创建；它往应用 visual 下挂子节点，改的是应用设备创建的对象，这就是文档要求“设置 `RootVisualTarget` 后在应用自己的设备上 `Commit`”的原因。

**附带副作用。** 进程和窗口都没设置 AppUserModelID 时，"during WebView creation a generated Application User Model ID is set to root window of parentWindow"（[ICoreWebView2Environment3::CreateCoreWebView2CompositionController](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2environment3#createcorewebview2compositioncontroller)）。替代开始菜单的工具窗口需要留意这一点。

### 1b. `Close()` 时 visual 树会怎样；能否复用同一个 visual；顺序要求

**文档写了什么。** `Close()`："Closes the WebView and cleans up the underlying browser instance. … After running `Close`, most methods will fail and event handlers stop running. … `Close` is synchronous and won't trigger the `beforeunload` event."（[ICoreWebView2Controller::Close](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2controller#close)）这里没有提 visual 树。唯一文档化的断开手段是把 `RootVisualTarget` 设为 null（[get_RootVisualTarget](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2compositioncontroller#get_rootvisualtarget)）。**`Close()` 会不会把 WebView2 挂进应用 visual 的子节点摘掉：未文档化。**

**微软示例的关闭顺序：先断开 visual、拆树，再 `Close()`。**

1. `AppWindow::CloseWebView` 先 `DeleteAllComponents()`，后 `m_controller->Close()`（[AppWindow.cpp L2655-L2740](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/AppWindow.cpp#L2655-L2740)）。
2. `ViewComponent` 析构时依次 `RevokeDragDrop`、`remove_CursorChanged`、`put_RootVisualTarget(nullptr)`、`DestroyDCompVisualTree()`（[ViewComponent.cpp L1066-L1096](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/ViewComponent.cpp#L1066-L1096)）。注释原文："If the webview closes because the AppWindow is closed (as opposed to being closed explicitly), this will no-op because in this case, the webview closes before the ViewComponent is destroyed. If the webview is closed explicitly, this will succeed."
3. `DestroyDCompVisualTree()` 对 webview visual 和根 visual 都调 `RemoveAllVisuals()`，再 `target->SetRoot(nullptr)`、释放 target、`Commit()`（[ViewComponent.cpp L924-L945](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/ViewComponent.cpp#L924-L945)）。

**示例从不复用 visual。** 每次 `InitializeWebView()` 都会关闭旧 WebView、丢弃旧设备、新建设备（[AppWindow.cpp L1816-L1831](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/AppWindow.cpp#L1816-L1831)）；新的 `ViewComponent` 构造时对同一个主窗口再次 `CreateTargetForHwnd`（[ViewComponent.cpp L908-L921](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/ViewComponent.cpp#L908-L921)）。这说明“在同一 HWND 上释放旧 target 后重建”是一条走得通的路，与 `CreateTargetForHwnd` 文档一致："When a composition target object is destroyed, the layer it composed is available for use by a new composition target object."（[IDCompositionDevice::CreateTargetForHwnd](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositiondevice-createtargetforhwnd)）

**同一个 `IDCompositionVisual` 能否给新 controller 当 `RootVisualTarget`：未文档化。** DComp 的规则是一个 visual 只能有一个父节点，或者作为一个 target 的根，`AddVisual` 遇到已有父节点的 visual 会失败（[AddVisual](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositionvisual-addvisual)，[IDCompositionTarget::SetRoot](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositiontarget-setroot)）。应用的 visual 一直留在应用的树里，WebView2 只往它下面挂子节点，所以从 DComp 规则看复用没有障碍（推断）。风险在于旧 WebView 的子 visual 如果没被摘掉，会和新 WebView 的子 visual 并存；示例对宿主 visual 调 `RemoveAllVisuals()`，正好覆盖这种情况。

**顺序要求。** 除了“设置后在应用设备上 `Commit`”，文档没有规定销毁和重建的顺序。照示例的做法，可以这样排（推断）：

1. 旧 controller：注销事件，`put_RootVisualTarget(nullptr)`；
2. 宿主 visual：`RemoveAllVisuals()`，应用设备 `Commit()`；
3. 处理拖放注册（见第 5 节）；
4. 旧 controller `Close()`；
5. 在同一 `parentWindow` 上创建新 composition controller，`put_RootVisualTarget(同一 visual)`，`Commit()`，再设 `Bounds`、`IsVisible`。

另外，若 `parentWindow` 在创建完成前被销毁，创建会以 `E_ABORT` 失败（[CreateCoreWebView2CompositionController](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2environment3#createcorewebview2compositioncontroller)）。

### 1c. `Bounds` 的参照系；`RasterizationScale`、`ShouldDetectMonitorScaleChanges`、`BoundsMode`

**`Bounds` 相对父 HWND。** "Bounds are relative to the parent `HWND`"，文档给了两种布局：子 HWND 用 `(0, 0)`，或者用顶层窗口并设置 `Bounds` 的左上角（[ICoreWebView2Controller::get_Bounds](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2controller#get_bounds)，[Overview: Sizing, positioning, and visibility](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/overview-features-apis#sizing-positioning-and-visibility)）。visual hosting 下："The app uses this visual to position the WebView within the app. The app still needs to use the Bounds property to size the WebView."（[get_RootVisualTarget](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2compositioncontroller#get_rootvisualtarget)）

**示例让 visual 偏移与 `Bounds` 原点保持一致。** `ResizeWebView()` 中 `put_Bounds` 的左上角取 `m_webViewBounds.left/top`，同时把根 visual 的 `OffsetX/Y` 设成同样的值，并按尺寸裁剪（[ViewComponent.cpp L572-L617](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/ViewComponent.cpp#L572-L617)）。（推断）visual hosting 下 WebView2 不会再按 `Bounds.left/top` 平移自己的内容，否则示例会被偏移两次。`Bounds` 左上角定义的是 WebView 客户区在父 HWND 客户区里的逻辑原点，鼠标坐标换算减的就是它（见 1d）。所以 visual 的实际位置应当与 `Bounds` 原点重合。

**祖先 visual 的缩放会让内容变糊。** "if a WebView2's ancestor Visual is scaled (zoomed) 2x, then the WebView2's contents are also rendered at 2x scale … But because the WebView2 isn't scaling its own contents, they're blurry." 文档建议撤销 visual 缩放，改用 Rasterization Scale API（[Requirements for scaling the contents](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/windowed-vs-visual-hosting#requirements-for-scaling-the-contents)）。（推断）对本项目来说，动画容器上的缩放会把 WebView 的位图一起缩放，WebView2 不会自动跟着改 `RasterizationScale`。

三个属性的语义（[ICoreWebView2Controller3](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2controller3)）：

- **`RasterizationScale`**：显示器 DPI 缩放与系统文本缩放的乘积，作用于网页内容以及 "popups, context menus, scroll bars"；常规的应用内缩放应该用 `ZoomFactor`。
- **`ShouldDetectMonitorScaleChanges`**：为 true 时 WebView 自己跟踪显示器 DPI，更新 `RasterizationScale` 并触发 `RasterizationScaleChanged`；为 false 时由应用自己设置，事件永远不会触发。
- **`BoundsMode`**：`COREWEBVIEW2_BOUNDS_MODE_USE_RAW_PIXELS` 时 `Bounds` 是原始屏幕像素，改 `RasterizationScale` 不改变像素尺寸；`COREWEBVIEW2_BOUNDS_MODE_USE_RASTERIZATION_SCALE` 时 `Bounds` 是逻辑尺寸，"Logical size * rasterization scale = Raw Pixel size"（另见 [COREWEBVIEW2_BOUNDS_MODE](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/webview2-idl#corewebview2_bounds_mode)）。

示例的自定义缩放做法是先 `put_ShouldDetectMonitorScaleChanges(FALSE)`，再把 `RasterizationScale` 设为“额外缩放 × DPI 缩放 × 文本缩放”（[ViewComponent.cpp L699-L711](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/ViewComponent.cpp#L699-L711)）。

参考页没有写 `ShouldDetectMonitorScaleChanges` 和 `BoundsMode` 的默认值（未文档化）。示例把 `put_ShouldDetectMonitorScaleChanges(TRUE)` 放在名为 `IDM_RASTERIZATION_SCALE_DEFAULT` 的菜单项下（[ViewComponent.cpp L303-L306](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/ViewComponent.cpp#L303-L306)），只能算旁证。

### 1d. `SendMouseInput` 与 `SendPointerInput`

**坐标空间：WebView 客户区坐标 = 父 HWND 客户区坐标 − `Bounds` 左上角，物理像素。** 文档："Point is expected to be in the client coordinate space of the WebView."（[SendMouseInput](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2compositioncontroller#sendmouseinput)），以及 "The app is also responsible for any necessary transformation of input positions into the WebView2's coordinate space."（[Requirements for routing spatial inputs](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/windowed-vs-visual-hosting#requirements-for-routing-spatial-inputs-mouse-touch-or-pen)）。示例的换算分三步（[ViewComponent.cpp L738-L841](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/ViewComponent.cpp#L738-L841)）：

1. 取消息里的客户区坐标；
2. `WM_MOUSEWHEEL`、`WM_MOUSEHWHEEL`、`WM_NCRBUTTONDOWN`、`WM_NCRBUTTONUP` 的坐标是屏幕坐标，先 `ScreenToClient(父 HWND)`；
3. 减去 `Bounds` 左上角（`WM_MOUSELEAVE` 不调整）。

示例没有做 DPI 换算。

**事件种类。** `COREWEBVIEW2_MOUSE_EVENT_KIND` 有 `MOVE`，`LEFT/RIGHT/MIDDLE_BUTTON_DOWN/UP/DOUBLE_CLICK`，`X_BUTTON_DOWN/UP/DOUBLE_CLICK`，`WHEEL`，`HORIZONTAL_WHEEL`，`LEAVE`，`NON_CLIENT_RIGHT_BUTTON_DOWN/UP`，每个都对应一个 `WM_*` 消息（[COREWEBVIEW2_MOUSE_EVENT_KIND](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/webview2-idl#corewebview2_mouse_event_kind)）。示例直接把 `WM_*` 消息号强转成这个枚举（[ViewComponent.cpp L825-L828](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/ViewComponent.cpp#L825-L828)），说明两者数值相同。`virtualKeys` 对应 `MK_*` 标志（[COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/webview2-idl#corewebview2_mouse_event_virtual_keys)），示例用 `GET_KEYSTATE_WPARAM(wParam)` 取得。

**`mouseData`。** 规则如下（[SendMouseInput](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2compositioncontroller#sendmouseinput)）：

- 滚轮："A positive value indicates that the wheel was rotated forward … One wheel click is defined as WHEEL_DELTA, which is 120."
- X 键：1 或 2，表示第几个 X 键。
- `LEAVE`："virtualKeys, mouseData, and point should all be zero"。
- 其他种类：0。

示例用 `GET_WHEEL_DELTA_WPARAM` 取带符号的 delta 后放进 `DWORD`（[ViewComponent.cpp L766-L776](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/ViewComponent.cpp#L766-L776)）。

**鼠标捕获和离开事件都由宿主负责。** 文档原文："To track mouse events that start in the WebView and can potentially move outside of the WebView and host application, calling SetCapture and ReleaseCapture is recommended. To dismiss hover popups, it is also recommended to send COREWEBVIEW2_MOUSE_EVENT_KIND_LEAVE messages."（[SendMouseInput](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2compositioncontroller#sendmouseinput)）示例的做法（[ViewComponent.cpp L777-L839](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/ViewComponent.cpp#L777-L839)）：

- 在 WebView 内按下任一键时 `SetCapture(父 HWND)`，抬起时 `ReleaseCapture`；
- 首次 `WM_MOUSEMOVE` 时 `TrackMouseEvent(TME_LEAVE)`，收到 `WM_MOUSELEAVE` 时转发 `LEAVE`；
- 光标移出 WebView 但仍在应用内时，取消跟踪并主动补发一次 `LEAVE`。

**`SendPointerInput` 只接受触摸和笔。** "SendPointerInput accepts touch or pen pointer input of types defined in COREWEBVIEW2_POINTER_EVENT_KIND. Any pointer input from the system must be converted into an ICoreWebView2PointerInfo first."（[SendPointerInput](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2compositioncontroller#sendpointerinput)）事件种类为 `ACTIVATE`/`DOWN`/`ENTER`/`LEAVE`/`UP`/`UPDATE`，对应 `WM_POINTER*`（[COREWEBVIEW2_POINTER_EVENT_KIND](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/webview2-idl#corewebview2_pointer_event_kind)）。

`ICoreWebView2PointerInfo` 的字段规则（[ICoreWebView2PointerInfo](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2pointerinfo)）：

- 它是 `POINTER_INFO`、`POINTER_TOUCH_INFO`、`POINTER_PEN_INFO` 的合体，去掉了 `HWND`、`HANDLE` 这类类型，`sourceDevice` 由 `PointerDeviceRect` 和 `DisplayRect` 代替；
- "any of the point or rect locations are expected to be in WebView physical coordinates. That is, coordinates relative to the WebView and no DPI scaling applied."；
- `PointerKind` 只支持 `PT_PEN` 和 `PT_TOUCH`。

空对象由 `ICoreWebView2Environment3::CreateCoreWebView2PointerInfo` 创建，"needs to be populated with all of the relevant info before calling SendPointerInput"（[CreateCoreWebView2PointerInfo](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2environment3#createcorewebview2pointerinfo)）。**哪些字段必填，文档没有列出（未文档化）。**

**示例用的是实验接口。** 示例没有手工填字段，而是调用 `ICoreWebView2ExperimentalCompositionController4::CreateCoreWebView2PointerInfoFromPointerId(pointerId, hwnd, transform, …)`，把“HWND 到 WebView 的 4×4 变换”（含 `Bounds` 偏移）交给 WebView2 换算；对从 WebView 内开始的指针，即使移出边界也继续转发（[ViewComponent.cpp L844-L890](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/ViewComponent.cpp#L844-L890)）。该接口是 "an experimental API that is shipped with our prerelease SDK"，只接受笔和触摸（[ICoreWebView2ExperimentalCompositionController4](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2experimentalcompositioncontroller4?view=webview2-1.0.4312-prerelease)）。本机 cargo 缓存里的 `webview2-com-sys 0.39.1` 没有绑定这个实验接口（本地检索结果），因此用 webview2-com 时要自己从 `GetPointerInfo`/`GetPointerTouchInfo`/`GetPointerPenInfo` 取值，并换算到 WebView 物理坐标（推断）。

**已知回归（和本项目的鼠标转发直接相关）：**

- [#5712](https://github.com/MicrosoftEdge/WebView2Feedback/issues/5712)（open）：Visual-DComp 下按住左键拖出窗口、反复几次后，`SendMouseInput` 仍返回 `S_OK`，`WM_NCHITTEST` 为 `HTCLIENT`，`GetNonClientRegionAtPoint` 返回 CLIENT，但页面不再响应，`ProcessFailed` 也不触发。运行时 152.0.4191.66，用官方 `WebView2APISample` 可复现。
- [#5695](https://github.com/MicrosoftEdge/WebView2Feedback/issues/5695)（open，regression）：同类现象，151 正常，152 容易复现。
- [#5713](https://github.com/MicrosoftEdge/WebView2Feedback/issues/5713)（closed）：152.0.4191.66+ composition 模式鼠标键盘失灵，被指向 [#5715](https://github.com/MicrosoftEdge/WebView2Feedback/issues/5715)（closed，无评论）。
- [#5696](https://github.com/MicrosoftEdge/WebView2Feedback/issues/5696)（open）：讨论 152 引入的 `app-region: drag` 继承变化。评论里有用户认为 composition 输入失灵与此无关；2026-10-26 有用户在 155.0.4283.62 上仍能复现，微软方 2026-10-30 回复 "we will provide an update soon"。

截至调研日，这组回归没有修复版本。

### 1e. 光标：`Cursor`、`SystemCursorId` 与 `CursorChanged`

- **`get_Cursor`** 返回 `HCURSOR`："The cursor should be set in WM_SETCURSOR through ::SetCursor or set on the corresponding parent/ancestor HWND of the WebView through ::SetClassLongPtr. The HCURSOR can be freed so CopyCursor/DestroyCursor is recommended to keep your own copy if you are doing more than immediately setting the cursor."（[get_Cursor](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2compositioncontroller#get_cursor)）
- **`get_SystemCursorId`** 在光标是系统默认光标时返回资源 ID（例如 `IDC_IBEAM`），"if custom CSS cursors are being used, this will return 0"；使用前要先 `MAKEINTRESOURCE`（[get_SystemCursorId](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2compositioncontroller#get_systemcursorid)）。
- **`CursorChanged`** 在 "WebView thinks the cursor should be changed" 时触发；"It is expected for the developer to send COREWEBVIEW2_MOUSE_EVENT_KIND_LEAVE messages (in addition to COREWEBVIEW2_MOUSE_EVENT_KIND_MOVE messages) through the SendMouseInput API. This is to ensure that the mouse is actually within the WebView that sends out CursorChanged events."（[add_CursorChanged](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2compositioncontroller#add_cursorchanged)）

示例在事件回调里取 `Cursor` 或 `SystemCursorId`，用 `SetClassLongPtr(GCLP_HCURSOR)` 设到主窗口；拿到空光标且没有错误时 `SetCursor(NULL)`（[ViewComponent.cpp L152-L193](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/ViewComponent.cpp#L152-L193)）。（推断）只用 `SystemCursorId` 会丢掉 CSS 自定义光标（此时返回 0），需要回退到 `get_Cursor` 并自行 `CopyCursor`。

---

## 2. visual hosting 下的键盘、焦点与 IME

### 2a. WebView2 是否有自己的 HWND；`MoveFocus` 做什么；键盘要不要转发

**应用只转发空间输入，不转发键盘。** hosting 概念页说 visual hosting 下 "Your host app takes spatial input (such as mouse or touch input) from the user"，并且 "no spatial inputs (such as mouse, touch, or pen) are sent to the WebView2 control, unless the app manages such input"（[Windowed vs. Visual hosting](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/windowed-vs-visual-hosting#requirements-for-routing-spatial-inputs-mouse-touch-or-pen)）。API 总览的 "Forwarding input" 一节也只提空间输入（[Overview: Forwarding input](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/overview-features-apis#forwarding-input)），"Keyboard accelerators" 一节则写 "When WebView2 has focus, it directly receives input from the user."（[Overview: Keyboard accelerators](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/overview-features-apis#keyboard-accelerators)）

微软成员 bradp0721 在 [#1054](https://github.com/MicrosoftEdge/WebView2Feedback/issues/1054)（2022-05-17）的回答把这一点说得最清楚：

> There isn't an API for keyboard input when using DirectComposition, because the WebView2 is able to get keyboard input directly from Windows. The app does not need to create a child HWND to host the WebView2. The WebView2 does need the parent window that is connected to the root of the DComp tree. Spatial input (mouse/pointer/pen events) are delivered to the HWND, which is why the app needs to receive these events and call the matching API on ICoreWebView2CompositionController. The WebView2 will take focus (either when received a pressed event or through the MoveFocus API) and by having focus, the OS will route keyboard events directly to WebView2. That is why there isn't a keyboard API.

所以应用不需要、也没有 API 可以转发 `WM_KEY*`、`WM_CHAR` 或 `WM_IME_*`。另外，`AllowHostInputProcessing`（让输入先经过宿主）"has no effect when using visual hosting"（[ICoreWebView2ControllerOptions4](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2controlleroptions4#put_allowhostinputprocessing)）；`AcceleratorKeyPressed` 在 "windowless mode" 下异步运行（[add_AcceleratorKeyPressed](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2controller#add_acceleratorkeypressed)）。

**WebView2 有没有自己的（隐藏）HWND、父/所有者是谁：未文档化。** 能找到的线索有两条：

- Win32 规则：键盘焦点总是落在某个 HWND 上，"The window with the keyboard focus is either the active window, or a child window of the active window."（[Keyboard Input: Keyboard Focus and Activation](https://learn.microsoft.com/en-us/windows/win32/inputdev/about-keyboard-input#keyboard-focus-and-activation)）。WebView2 既然能“拿到焦点”并由系统直接投递键盘消息，它一定有一个可获得焦点的窗口（推断）。
- `put_ParentWindow` 说它会 "re-parent the main WebView window to the newly provided window"（[put_ParentWindow](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2controller#put_parentwindow)），但这句话是窗口模式和 visual 模式共用的。

该窗口的类名、所属进程以及父子或所有者关系，都没有一手说明。由上面的 Win32 规则还可推出：宿主顶层窗口必须是活动窗口，WebView2 的焦点窗口才能收到键盘和 IME（推断）。对 `WS_EX_TOOLWINDOW | WS_EX_TOPMOST` 的弹窗来说，uncloak 之后要真正激活它。

**`MoveFocus`。** "WebView gets focus and focus is set to correspondent element in the page hosted in the WebView. For Programmatic reason, focus is set to previously focused element or the default element if no previously focused element exists."（[MoveFocus](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2controller#movefocus)）文档没有给 visual hosting 单独的说明。结合 #1054，WebView2 收到按下事件时已经会自己取得焦点，点击时再调 `MoveFocus` 不是文档要求的步骤（推断）。如果保留，放在转发按下事件**之前**更稳妥，以免 `PROGRAMMATIC` 把焦点恢复到上一个元素，覆盖这次点击的结果（推断）。

### 2b. IME 候选窗如何定位；祖先 visual 有偏移/变换会怎样

**WebView2 文档没有讲 visual hosting 下 IME 候选窗或组字窗怎么定位（未文档化）。** 相关的只有三条：

- `Bounds` 相对父 HWND（见 1c）；
- `NotifyParentWindowPositionChanged` 用于告诉 WebView "the main WebView parent (or any ancestor) `HWND` moved"，"This is needed for accessibility and certain dialogs in WebView to work correctly."（[NotifyParentWindowPositionChanged](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2controller#notifyparentwindowpositionchanged)）；
- `RasterizationScale` 会影响 popups 和 context menus（见 1c）。

**没有任何 API 能把祖先 visual 的偏移或变换告诉 WebView2。** 唯一带变换矩阵的是实验性的指针换算函数，而它只用于指针输入（[ICoreWebView2ExperimentalCompositionController4](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2experimentalcompositioncontroller4?view=webview2-1.0.4312-prerelease)）。

**上游 Chromium 怎么算候选窗坐标：**

- TSF 的 `TSFTextStore::GetTextExt` 用 `TextInputClient::GetCaretBounds()` 或 `GetCompositionCharacterBounds()` 得到 DIP 屏幕坐标，再用 `DIPToScreenRect(window_handle_, …)` 转成屏幕坐标交给 TSF；`GetWnd` 返回同一个 `window_handle_`（[tsf_text_store.cc L359-L475](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/ui/base/ime/win/tsf_text_store.cc#359)）。
- 接口注释写明这些 bounds 是 "universal screen coordinates in DIP"（[text_input_client.h L168-L171、L225-L231](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/ui/base/ime/text_input_client.h#168)）。
- `RenderWidgetHostViewAura::GetCaretBounds()` 用 aura 的 `ScreenPositionClient` 把视图坐标换成屏幕坐标，并用 `AdjustToFit(window_->GetBoundsInScreen())` 把光标框夹在视图的屏幕矩形内（[render_widget_host_view_aura.cc L1781-L1797](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/content/browser/renderer_host/render_widget_host_view_aura.cc#1781)，[L1818-L1840](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/content/browser/renderer_host/render_widget_host_view_aura.cc#1818)）。
- `GetTextExt` 里专门处理了 "Pinyin IME of Windows 8" 在 `acp_start == acp_end` 时的调用（[tsf_text_store.cc L401-L404](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/ui/base/ime/win/tsf_text_store.cc#401)），说明微软拼音走的就是这条路径。

**推论（推断）。** 候选窗的位置取决于 Chromium 认为“视图在屏幕上的哪里”，而不是 DComp 实际把 visual 画在哪里。visual hosting 下 WebView2 能拿到的定位信息只有父 HWND 的屏幕位置、`Bounds` 和 `RasterizationScale`，由此有两点要求：

- 父 HWND 移动后要调 `NotifyParentWindowPositionChanged`（示例在 `WM_MOVE`/`WM_MOVING` 中调用，[ViewComponent.cpp L406-L412](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/ViewComponent.cpp#L406-L412)）；它只覆盖 HWND 的移动，不覆盖 visual 的偏移。
- 动画容器上的偏移、缩放或 3D 变换不会反映到候选窗、`<select>` 下拉框、右键菜单或无障碍矩形上。动画结束时 visual 的实际位置必须与 `Bounds` 原点重合；输入框有焦点时，不要让容器停在非零偏移上。

**旁证（非 visual hosting）。** [#5448](https://github.com/MicrosoftEdge/WebView2Feedback/issues/5448)（open）中，DPI 不感知的应用在 Window-to-Visual 模式下，右键菜单和 `<select>` 弹层在非 100% 缩放的屏幕上发生偏移。这说明这些弹层的位置是 WebView2 自己按宿主几何和缩放算出来的。

### 2c. WebView2Feedback 中与 IME / 候选窗 / 组字 / TSF 相关的 issue

检索方式：用 `gh search issues --repo MicrosoftEdge/WebView2Feedback` 搜了 "IME composition"、"IME CompositionController"、"IME visual hosting"、"candidate window"、"IME windowless"、"TSF"、"IME DirectComposition"、"Chinese IME"、"Japanese IME"、"IME position"、"IME"、"Pinyin"、"composition mode"、"visual hosting"、"MoveFocus composition"、"keyboard input visual hosting" 等关键词，并逐条读了正文和评论。"IME visual hosting"、"CompositionController IME"、"IME windowless"、"IME DirectComposition" 等没有额外结果。

与 visual hosting / composition 模式直接相关的：

- **[#2758](https://github.com/MicrosoftEdge/WebView2Feedback/issues/2758) "Input Method (IME) doesn't work in most cases after using ICoreWebView2CompositionController"**
  - 状态：open（bug, tracked），2022-09。
  - 现象：在 WinComp 示例的 composition 模式下 IME 不工作。微软 contributor 复现后指出，只有首个页面是项目内的本地 HTML 时才出现；若首个页面是带输入框的网站（如 bing.com），之后导航到哪里 IME 都正常。已开内部 bug。报告者版本为 SDK 1.0.1343.22 / runtime 105.0.1343.27。
  - 修复版本：无。官方 workaround：无。
- **[#4655](https://github.com/MicrosoftEdge/WebView2Feedback/issues/4655) "SampleWinComp Example Consistently Reproduces Browser Process Crash with ICoreWebView2CompositionController Embedding"**
  - 状态：closed（bug），2024-07 至 2024-08-05。
  - 现象：composition 模式下，焦点在不可编辑元素上时用微软拼音输入，浏览器进程崩溃。
  - 修复版本：微软 contributor 先说修复会进 Edge 129，随后更正为 "this should be fixed with the latest version of WebView2 runtime … 127.0.xxxx.xx"。
- **[#5570](https://github.com/MicrosoftEdge/WebView2Feedback/issues/5570) "IME candidate window position is incorrect in composition mode during Remote Desktop sessions"**
  - 状态：open，2026-04。
  - 现象：只在 composition 模式出现。远程桌面会话中候选窗错位，断开 RDP 回到本地后恢复；会话中重建 WebView 也没用。
  - 修复版本、官方回复、workaround：都没有。

窗口模式或其他框架、但属于同一类定位问题的：

- **[#5675](https://github.com/MicrosoftEdge/WebView2Feedback/issues/5675) "IME composition window jumps to the top-left of the screen after the host window is resized"**
  - 状态：open，2026-08。
  - 环境：**窗口模式**，标准 Tauri 2.11.5 / wry 0.55.1，runtime 151.0.4129.86，韩文 IME。
  - 现象：resize 后组字窗出现在屏幕左上角，直到移动窗口或点击别处才恢复。报告者试过 resize 后调 `MoveFocus(PROGRAMMATIC)`、多次调 `NotifyParentWindowPositionChanged()`，都无效；Edge 浏览器不复现。
  - 官方回复：无。
- **[#2241](https://github.com/MicrosoftEdge/WebView2Feedback/issues/2241) "Microsoft IME candidates window open in wrong place"**
  - 状态：open（bug, tracked），2022-03，WinUI 3。
  - 现象（报告者描述）：DPI > 100% 时总是错位；拖动窗口但不点击 WebView2 时，候选窗留在旧位置。
  - 官方：微软成员已加入 backlog。
- **[#1611](https://github.com/MicrosoftEdge/WebView2Feedback/issues/1611) "The 'Candidate' window of IME is not moving along with the cursor position"**
  - 状态：open（bug, tracked），2021-08，WPF。
  - 现象：候选窗打开后拖动应用窗口，候选窗不跟着移动。
  - 官方：已进 backlog。

中文输入质量和 TSF：

- **[#1610](https://github.com/MicrosoftEdge/WebView2Feedback/issues/1610) "Japanese / Chinese characters are getting duplicated while inputting using Japanese/Chinese IME Keyboard"**
  - 状态：closed（bug, tracked）。
  - 修复版本：微软成员 2024-09-05 回复 "This is fixed in runtimes 125+."
- **[#5625](https://github.com/MicrosoftEdge/WebView2Feedback/issues/5625) "WebView2 drops first CJK IME character & doubles Chinese punctuation in contentEditable (149.0.4022.69)"**
  - 状态：open，2026-06，Tauri 2。
  - 现象：`contentEditable` 中首字丢失，全角标点要按两次；`<textarea>` 正常；搜狗拼音严重，微软拼音较轻。
  - 2026-10-05 补充：154.0.4258.53 上空闲时正常；渲染主线程忙时（例如启动期间），搜狗仍会在提交后丢掉下一个首字母。
  - 官方回复：无。
- **[#5730](https://github.com/MicrosoftEdge/WebView2Feedback/issues/5730) "Runtime 153+: UI thread blocks ~33 s on every focus change when host app is the Windows shell (no Explorer)"**
  - 状态：open（bug, regression），2026-09。
  - 环境：WPF 窗口模式，宿主作为 Windows shell 运行，会话中没有 Explorer。
  - 现象：每次焦点变化，浏览器 UI 线程阻塞约 33 秒，栈在 `uiautomationcore → tiptsf → msctf`。`--disable-features=TSFImeSupport` 无效。151 正常，报告者回退到 152。
  - 适用范围：只有在没有 Explorer 的会话里才相关。

另见 [#1054](https://github.com/MicrosoftEdge/WebView2Feedback/issues/1054)（closed，feature request），确认 composition 模式没有键盘 API（见 2a）。

---

## 3. 可见性、cloak 与遮挡

### 3a. `IsVisible = false` 的效果

- 文档原文："If `IsVisible` is set to `FALSE`, the WebView2 is transparent and is not rendered. However, this does not affect the window containing the WebView2 …"；"There are CPU and memory benefits when the page is hidden. For instance, Chromium has code that throttles activities on the page like animations and some tasks are run less frequently. Similarly, WebView2 will purge some caches to reduce memory usage."。文档建议在窗口最小化时把它设为 false（[get_IsVisible](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2controller#get_isvisible)）。
- `TrySuspend` 要求 `IsVisible` 为 false，否则返回 `HRESULT_FROM_WIN32(ERROR_INVALID_STATE)`。挂起会暂停脚本定时器和动画，WebView 变为可见时自动恢复（[ICoreWebView2_3::TrySuspend](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2_3#trysuspend)）。

**页面可见性跟随 `IsVisible`，不跟随宿主窗口状态（推断）。** 文档用的说法是 "the page is hidden"（同上）。WebView2Feedback 的 [#2681](https://github.com/MicrosoftEdge/WebView2Feedback/issues/2681)（closed，tracked/stale）和 [#4879](https://github.com/MicrosoftEdge/WebView2Feedback/issues/4879)（open，bug）报告：宿主窗口最小化或被完全遮挡时，`visibilitychange` 不会触发。#2681 的分诊回复是 "currently, it's by design"，建议在窗口状态变化时改变 WebView2 容器的可见性，也就是去驱动 `IsVisible`。这些分诊账号在 GitHub 上显示为非组织成员，属于微软方回复，但不是正式文档。

**风险旁证（窗口模式）。** [#5673](https://github.com/MicrosoftEdge/WebView2Feedback/issues/5673)（open）：同一父窗口下的多个 controller 以 `IsVisible=FALSE` 创建，加载完成后再设为 `TRUE`，内容却不上屏，直到别的事件触发重绘。visual hosting 下是否同样存在，未知。

### 3b. Chromium 原生遮挡跟踪如何对待被 DWM cloak 的窗口；是否适用于 WebView2；相关开关

#### 上游 Chromium 的行为

**特性开关。**

- `kCalculateNativeWinOcclusion` 默认开启；`kApplyNativeOcclusionToCompositor` 默认关闭，它的类型参数有 `release`、`throttle`、`throttle_and_release`（[ui_base_features.cc L38-L50](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/ui/base/ui_base_features.cc#38)，[L74-L86](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/ui/base/ui_base_features.cc#74)）。
- 两参数形式的 `BASE_FEATURE(kX, …)` 的特性名就是去掉 `k` 的标识符，所以命令行里写 `CalculateNativeWinOcclusion`（[base/feature.h L70-L84](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/base/feature.h#70)）。
- 跟踪只对 `host->IsNativeWindowOcclusionEnabled()` 为真的 `WindowTreeHost` 启用（[native_window_occlusion_tracker.cc L18-L27](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/ui/aura/native_window_occlusion_tracker.cc#18)）。

**被跟踪的根窗口怎么判定。** 规则依次是（[native_window_occlusion_tracker_win.cc L566-L590](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/ui/aura/native_window_occlusion_tracker_win.cc#566)）：

1. `::IsIconic` → `HIDDEN`；
2. 否则若 `IsWindowOnCurrentVirtualDesktop() == false` → `OCCLUDED`；
3. 否则按几何计算。

`IsWindowOnCurrentVirtualDesktop` 的第一步是 "If the window is not cloaked, it is not on another desktop."；只有窗口被 cloak 时，才去问 `IVirtualDesktopManager::IsWindowOnCurrentVirtualDesktop`（[native_window_occlusion_tracker_win.cc L1033-L1048](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/ui/aura/native_window_occlusion_tracker_win.cc#1033)，[hwnd_util.cc L293-L314](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/ui/gfx/win/hwnd_util.cc#293)）。

**遮挡者的判定。** `IsWindowVisibleAndFullyOpaque` 会排除这些窗口：不可见、最小化、`WS_EX_TRANSPARENT`、`WS_EX_TOOLWINDOW`（任务栏除外）、非不透明的 layered 窗口、复杂区域、被 cloak 的窗口、空矩形、tooltip（[hwnd_util.cc L180-L291](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/ui/gfx/win/hwnd_util.cc#180)）。其中关于 cloak 的注释写着 "Windows 10 has cloaked windows, windows with WS_VISIBLE attribute but not displayed."（[L243-L249](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/ui/gfx/win/hwnd_util.cc#243)）

**其他触发条件。**

- 监听 `EVENT_OBJECT_CLOAKED`/`EVENT_OBJECT_UNCLOAKED` 并据此重算（[native_window_occlusion_tracker_win.cc L750-L752](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/ui/aura/native_window_occlusion_tracker_win.cc#750)）；
- aura 窗口 `!IsVisible()` → `HIDDEN`；锁屏或熄屏 → `OCCLUDED`（[L253-L268](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/ui/aura/native_window_occlusion_tracker_win.cc#253)）。

**遮挡状态怎么影响页面。** `WebContentsViewAura::OnWindowOcclusionChanged` 调 `UpdateWebContentsVisibility`（[web_contents_view_aura.cc L1035-L1049](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/content/browser/web_contents/web_contents_view_aura.cc#1035)，[L1485-L1489](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/content/browser/web_contents/web_contents_view_aura.cc#1485)）。在 `WebContentsImpl::UpdateWebContentsVisibility` 里，命令行带 `disable-backgrounding-occluded-windows` 时会把 `OCCLUDED` 改成 `VISIBLE`，`HIDDEN` 不受影响（[web_contents_impl.cc L12747-L12772](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/content/browser/web_contents/web_contents_impl.cc#12747)）。

**推论（推断，基于以上源码）。** 在上游 Chrome 里，一个被应用自己 cloak（`DWM_CLOAKED_APP`）、但仍在当前虚拟桌面上的窗口，其状态取决于 `IVirtualDesktopManager` 怎么回答：

- 回答“在当前桌面”：Chromium 只按几何判断，cloak 本身不会让它变成 hidden 或 occluded；
- 回答“不在”：判为 `OCCLUDED`。

`IVirtualDesktopManager` 对应用自行 cloak 的窗口会返回什么，本次读到的一手资料没有说明。

#### 是否适用于 WebView2

- WebView2 是否对宿主窗口（或 visual hosting 下的任何窗口）启用这套跟踪：**未文档化**，嵌入层闭源，visual hosting 也没有专门说明。
- 旁证 1：#2681 和 #4879 显示，宿主被最小化或遮挡时页面不会变成 hidden（窗口模式），这与“WebView2 不靠原生遮挡决定页面可见性”一致（推断）。
- 旁证 2：[#5669](https://github.com/MicrosoftEdge/WebView2Feedback/issues/5669) 的报告者在窗口模式下通过 `AdditionalBrowserArguments` 传了 `--disable-features=CalculateNativeWinOcclusion`，对他那个与遮挡无关的上屏问题“no effect”。这只能说明该开关能被接受，不能说明它起了作用。

#### 相关开关

- **`--disable-features=CalculateNativeWinOcclusion`**：关闭原生遮挡计算（上游语义见上）。
- **`--disable-backgrounding-occluded-windows`**：源码注释为 "Disable backgrounding renders for occluded windows. Done for tests to avoid nondeterministic behavior."（[content_switches.cc L90-L93](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/content/public/common/content_switches.cc#90)），作用只是把 `OCCLUDED` 视为 `VISIBLE`。
- **`--disable-renderer-backgrounding`**："Renderer processes keep a foreground priority at the OS level, and renderers are never told that they are backgrounded or hidden."（[content_switches.cc L236-L243](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/content/public/common/content_switches.cc#236)）
- **`--disable-background-timer-throttling`**："Disable task throttling of timer tasks from background pages."（[content_switches.cc L95-L97](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/content/public/common/content_switches.cc#95)）。这是四个开关里**唯一**出现在 WebView2 官方 flags 列表中的（[WebView2 browser flags](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/webview-features-flags)）。

同一 flags 文档还写道："Apps in production shouldn't use WebView2 browser flags"，以及 "Chromium flags are not owned or controlled by Microsoft Edge"（[WebView2 browser flags](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/webview-features-flags)）。

#### 宿主怎么传参

- **代码传参**：用 `ICoreWebView2EnvironmentOptions::put_AdditionalBrowserArguments`。"Please note that calling this API twice will replace the previous value rather than appending to it. If there are multiple switches, there should be a space in between them. The one exception is if multiple features are being enabled/disabled for a single switch, in which case the features should be comma-seperated."（[put_AdditionalBrowserArguments](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2environmentoptions#put_additionalbrowserarguments)）
- **合并规则**："If a switch is specified multiple times, only the last instance is used." 但 "A merge of the different values of the same switch is not attempted, except for disabled and enabled features. … The features is the union of the specified features and built-in features. If a feature is disabled, it is removed from the enabled features list."（[get_AdditionalBrowserArguments](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2environmentoptions#get_additionalbrowserarguments)）
- **环境变量和注册表**：`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` 或策略注册表里的 `AdditionalBrowserArguments`，"it is appended to the corresponding values"，是追加而不是替换（[CreateCoreWebView2EnvironmentWithOptions](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/webview2-idl#createcorewebview2environmentwithoptions)）。以提升权限运行的应用会忽略环境里设置的 flags（[WebView2 browser flags](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/webview-features-flags)）。
- **共享浏览器进程**：选项与正在运行的 WebView 不一致时，创建以 `HRESULT_FROM_WIN32(ERROR_INVALID_STATE)` 失败（[CreateCoreWebView2EnvironmentWithOptions](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/webview2-idl#createcorewebview2environmentwithoptions)）。

**wry 的情况。**

- 只有调用方没给 `additional_browser_args` 时，wry 才使用默认的 `--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection`（外加 autoplay、proxy 参数）；一旦调用方给了就整体替换（[wry v0.57.0 src/webview2/mod.rs L299-L333](https://github.com/tauri-apps/wry/blob/wry-v0.57.0/src/webview2/mod.rs#L299-L333)）。
- wry 0.57.0 自己的 WebView2 后端只调用 `CreateCoreWebView2Controller(WithOptions)`（[L382-L423](https://github.com/tauri-apps/wry/blob/wry-v0.57.0/src/webview2/mod.rs#L382-L423)），仓库里也搜不到 composition controller 代码（GitHub 代码搜索 `CompositionController`、`RootVisualTarget` 均无结果）。因此本项目的 visual hosting 环境如果是自己创建的，wry 的默认参数根本不会生效（推断）。
- 如果要加遮挡相关开关，又想保留 wry 原有的行为，应写成一个开关：`--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,CalculateNativeWinOcclusion`（推断，依据上面的合并规则）。

### 3c. DWM：`DWMWA_CLOAK` 与 `DWMWA_TRANSITIONS_FORCEDISABLED`

**`DWMWA_CLOAK` 的语义。** "Use with DwmSetWindowAttribute. Cloaks the window such that it is not visible to the user. The window is still composed by DWM." 在 DComp 场景下的用法是 "Use the DWMWA_CLOAK flag to cloak the layered child window when animating a representation of the window's content via a DirectComposition visual that has been associated with the layered child window."；Windows 7 及更早不支持（[DWMWINDOWATTRIBUTE](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ne-dwmapi-dwmwindowattribute)）。对应的教程在动画期间 cloak layered child window，动画结束后 uncloak（[How to animate the bitmap of a layered child window](https://learn.microsoft.com/en-us/windows/win32/directcomp/how-to--animate-the-bitmap-of-a-layered-child-window)）。

**`DWMWA_CLOAKED`（只读）。** 取值有 `DWM_CLOAKED_APP`（0x1，"The window was cloaked by its owner application"）、`DWM_CLOAKED_SHELL`（0x2）、`DWM_CLOAKED_INHERITED`（0x4，"The cloak value was inherited from its owner window"）（[DWMWINDOWATTRIBUTE](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ne-dwmapi-dwmwindowattribute)）。（推断）被宿主拥有（owned）的窗口会继承 cloak；不被拥有的顶层窗口（例如别的进程的顶层窗口）不会继承。

**DWM 仍然合成被 cloak 的窗口**："The window is still composed by DWM."（同上）

**谁可以 cloak：未文档化。** `DwmSetWindowAttribute` 文档没有说明跨进程或跨线程的限制（[DwmSetWindowAttribute](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/nf-dwmapi-dwmsetwindowattribute)）。

**被 cloak 的窗口是否参与命中测试、能否收到鼠标：Win32 文档没有说明（未文档化）。** 能找到的相关表述有：

- `EVENT_OBJECT_CLOAKED` 的说明只有 "A cloaked window still exists, but is invisible to the user."（[Event Constants](https://learn.microsoft.com/en-us/windows/win32/winauto/event-constants)）。
- 唯一明确谈到输入的一手资料是 RDP RAIL 协议规范："A cloaked window MUST be present on the client, but MUST be invisible to the user and MUST NOT receive mouse, pen, or touch input."（[MS-RDPERP: Sending Window Cloak State Change PDU](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdperp/17c30a1c-4a18-44b5-8b5f-7b1f1ce0f805)）。这是对 RAIL 客户端实现的要求，不是通用的 Win32 契约。
- Chromium 的注释指出 cloak 的窗口仍带 `WS_VISIBLE`（[hwnd_util.cc L243-L249](https://chromium.googlesource.com/chromium/src/+/cc7cf7c72540cbde2351186331a953d704942349/ui/gfx/win/hwnd_util.cc#243)）。

**非官方观测（终端用户诊断）。** [#5668](https://github.com/MicrosoftEdge/WebView2Feedback/issues/5668)（open）中，WhatsApp（基于 WebView2）的宿主窗口在其他虚拟桌面上被 shell cloak（`DWMWA_CLOAKED = 2`），但 msedgewebview2.exe 里的 `Chrome_WidgetWin_1` 顶层窗口是 `DWMWA_CLOAKED = 0`：它带着 `WS_EX_LAYERED`（alpha 0）、`WS_EX_NOACTIVATE`、`WS_EX_TOOLWINDOW`、`WS_EX_NOREDIRECTIONBITMAP` 留在桌面上，继续参与命中测试，吞掉了桌面点击。宿主用的是哪种 hosting 模式不清楚，但这提示 WebView2 可能有不随宿主一起 cloak 的自有顶层窗口；本项目应实际枚举窗口确认（推断）。

**`DWMWA_TRANSITIONS_FORCEDISABLED`。** "Use with DwmSetWindowAttribute. Enables or forcibly disables DWM transitions. The pvAttribute parameter points to a value of type BOOL. TRUE to disable transitions, or FALSE to enable transitions."（[DWMWINDOWATTRIBUTE](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ne-dwmapi-dwmwindowattribute)）它与 cloak/uncloak 怎么交互，文档没有说明（未文档化）。

---

## 4. DirectComposition

### 4a. `IDCompositionDevice::CreateTargetForHwnd` 的限制

- **HWND 必须属于调用进程。** "in all cases the window must belong to the calling process. If the window belongs to a different process, this method returns DCOMPOSITION_ERROR_ACCESS_DENIED."（[CreateTargetForHwnd](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositiondevice-createtargetforhwnd)，[DirectComposition error codes](https://learn.microsoft.com/en-us/windows/win32/directcomp/directcomposition-error-codes)）文档只要求同进程，**没有说必须同线程（未文档化）**。
- **每个窗口最多两个 target。** "At most, only two composition targets can be created for each window in the system, one topmost and one not topmost. If a composition target is already bound to the specified window at the specified layer, this method fails."（[CreateTargetForHwnd](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositiondevice-createtargetforhwnd)）对应错误码是 `DCOMPOSITION_ERROR_WINDOW_ALREADY_COMPOSED`："The IDCompositionDevice::CreateTargetForHwnd method was called with hwnd and topmost parameters for which a visual tree already exists."（[DirectComposition error codes](https://learn.microsoft.com/en-us/windows/win32/directcomp/directcomposition-error-codes)）原文说的是 "in the system"，据此理解，限制按窗口算，不按设备算。
- **target 何时释放。** "When a composition target object is destroyed, the layer it composed is available for use by a new composition target object."（[CreateTargetForHwnd](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositiondevice-createtargetforhwnd)）要不要先 `Commit` 才释放该层：未文档化。示例的顺序是 `SetRoot(nullptr)` → 释放 → `Commit()`（[ViewComponent.cpp L924-L945](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/ViewComponent.cpp#L924-L945)）。
- **四层模型。** 从下到上是：窗口自身内容 → 非 topmost 的 DComp 树 → 子窗口 → topmost 的 DComp 树，全部裁剪到窗口的可见区域；`topmost=TRUE` 让 visual 树显示在子窗口之上（[CreateTargetForHwnd](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositiondevice-createtargetforhwnd)）。
- **冲突来源。** WebView2 文档描述的是“把 WebView 的树连到应用提供的 visual 上”（[get_RootVisualTarget](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2compositioncontroller#get_rootvisualtarget)），没有提到它会对 `parentWindow` 创建 target。（推断）同一 HWND 上的 target 冲突应来自应用自身（或别的组件）重复创建。

### 4b. `WS_EX_NOREDIRECTIONBITMAP`

**定义。** "The window does not render to a redirection surface. This is for windows that do not have visible content or that use mechanisms other than surfaces to provide their visual."（[Extended Window Styles](https://learn.microsoft.com/en-us/windows/win32/winmsg/extended-window-styles)）

**为什么只用 DComp 的透明窗口需要它（推断，由两条文档拼出）：**

- `CreateTargetForHwnd` 的分层模型里，窗口自身内容在最底层（[CreateTargetForHwnd](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositiondevice-createtargetforhwnd)）；
- `DWMWA_REDIRECTIONBITMAP_ALPHA`（Windows 11 26100 起）的说明是重定向位图默认 "the alpha is ignored and the redirection bitmap is treated as fully opaque"（[DWMWINDOWATTRIBUTE](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ne-dwmapi-dwmwindowattribute)）。

所以带重定向位图的窗口，会有一层不透明的底层挡在透明的 DComp 内容后面；去掉重定向表面后，DComp 内容才能直接透出桌面。

**能否在创建后修改：未文档化。** 文档只说这些扩展样式 "can be used along with the CreateWindowExA/ CreateWindowExW functions"（[Extended Window Styles](https://learn.microsoft.com/en-us/windows/win32/winmsg/extended-window-styles)）；`SetWindowLongPtr` 也只有一条通用说明："Certain window data is cached, so changes you make using SetWindowLongPtr will not take effect until you call the SetWindowPos function."（[SetWindowLongPtrW](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowlongptrw)）。应在 `CreateWindowEx` 时就设好。

### 4c. 用 `IDCompositionAnimation` 动画偏移和不透明度（要点）

- **创建与构造。** 用 `IDCompositionDevice::CreateAnimation` 创建 `IDCompositionAnimation`，按开始时间递增追加 `AddCubic`、`AddSinusoidal`、`AddRepeat` 段，用 `End` 给出终值；调用 `End` 之后，除 `Reset` 外的方法都会失败（[Animation (DirectComposition)](https://learn.microsoft.com/en-us/windows/win32/directcomp/animation)，[IDCompositionAnimation](https://learn.microsoft.com/en-us/windows/win32/api/dcompanimation/nn-dcompanimation-idcompositionanimation)）。
- **偏移。** `IDCompositionVisual::SetOffsetX`/`SetOffsetY(IDCompositionAnimation*)`："This method makes a copy of the specified animation"，会替换先前的动画；动画对象必须由创建该 visual 的同一设备创建，不能是自定义实现（[SetOffsetX(IDCompositionAnimation)](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositionvisual-setoffsetx%28idcompositionanimation%29)）。
- **不透明度。** 两种做法：`IDCompositionEffectGroup::SetOpacity(IDCompositionAnimation*)`，同样复制动画、要求同一设备（[IDCompositionEffectGroup::SetOpacity](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositioneffectgroup-setopacity%28idcompositionanimation%29)）；或者用 Windows 8.1 起 `IDCompositionVisual3` 直接提供的 `SetOpacity`（值和动画两种重载）（[IDCompositionVisual3](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nn-dcomp-idcompositionvisual3)）。
- **时序。** 动画由合成引擎在独立线程上驱动；除非另行设定，"an animation function starts when the Desktop Window Manager (DWM) receives the command to execute the animation"（[Animation (DirectComposition)](https://learn.microsoft.com/en-us/windows/win32/directcomp/animation)）。
- **`Commit` 语义。** "Calls to DirectComposition methods are always batched and executed atomically as a single transaction. Calls take effect only when IDCompositionDevice::Commit is called"；使用多个设备时要分别 `Commit`，"the batch of commands might not take effect at the same time"（[IDCompositionDevice::Commit](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositiondevice-commit)）。

（推断）WebView2 的内容由它自己的设备提交（见 1a），应用对动画容器的提交和 WebView2 的内容帧不在同一个事务里；动画期间 WebView2 的逻辑位置（`Bounds`）也不会跟着 visual 移动（见 2b）。

---

## 5. visual hosting 下的拖放

- **宿主必须转发 `IDropTarget` 事件。** "Dragging from a WebView2 control to another application is supported by default. However, dragging to a WebView2 control requires that when the host app receives an `IDropTarget` event from the system, the host app must forward the event to the WebView2 control. Dragging to a WebView2 control includes drag-and-drop operations that are entirely within a WebView2 control."（[Overview: Drag and drop](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/overview-features-apis#drag-and-drop)）
- **`ICoreWebView2CompositionController3`** 的 `DragEnter`/`DragOver`/`DragLeave`/`Drop` 与 `IDropTarget` 的同名方法一一对应（[ICoreWebView2CompositionController3](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2compositioncontroller3)）：
  - "The hosting application must register as an IDropTarget and implement and forward … calls to this function"；
  - `point` "must be modified to include the WebView's offset and be in the WebView's client coordinates (Similar to how SendMouseInput works)"；
  - `AllowExternalDrop` 为 false 时返回 `E_FAIL`。`AllowExternalDrop` 默认为 TRUE（[ICoreWebView2Controller4::put_AllowExternalDrop](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2controller4#put_allowexternaldrop)）。
- **示例的做法。** `DropTarget::Init` 对主窗口（即 `parentWindow`）调 `RegisterDragDrop`；每个回调先 `ScreenToClient`，再减去 `Bounds` 原点，然后转发（[DropTarget.cpp L21-L68](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/DropTarget.cpp#L21-L68)，[ViewComponent.cpp L548-L553](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/ViewComponent.cpp#L548-L553)）；`ViewComponent` 析构时 `RevokeDragDrop`（[ViewComponent.cpp L1081-L1085](https://github.com/MicrosoftEdge/WebView2Samples/blob/8f5ad22c39f3b2825c9a1f23cb0fa4e1cb13e4c8/SampleApps/WebView2APISample/ViewComponent.cpp#L1081-L1085)）。
- **`RegisterDragDrop` 的约束**（[RegisterDragDrop](https://learn.microsoft.com/en-us/windows/win32/api/ole2/nf-ole2-registerdragdrop)）：
  - 已经注册过时返回 `DRAGDROP_E_ALREADYREGISTERED`；
  - "If you use CoInitialize or CoInitializeEx instead of OleInitialize to initialize COM, RegisterDragDrop will always return an E_OUTOFMEMORY error"；
  - 调用线程必须泵消息。
- **重建时的处理（推断）。** 在同一 HWND 上重建 WebView 时，要么保留同一个 `IDropTarget`、只替换它内部指向的 controller，要么先 `RevokeDragDrop` 再重新注册。

## 6. visual hosting 下的无障碍

默认情况下（Win32/C++），"WebView2 will show up in the accessibility tree as a child of the parent HWND"，同时 WebView2 提供 API，让应用把 WebView2 内容放到相对其他元素更合适的位置（[Overview: Accessibility](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/overview-features-apis#accessibility)）。这样的 API 有两个：一是 `ICoreWebView2CompositionController2::get_AutomationProvider`，返回的对象 "implements IRawElementProviderSimple"（[ICoreWebView2CompositionController2](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2compositioncontroller2#get_automationprovider)）；二是 `ICoreWebView2Environment4::GetAutomationProviderForWindow(hwnd)`："Host apps are expected to implement IRawElementProviderHwndOverride. When GetOverrideProviderForHwnd is called, the app can pass the HWND to GetAutomationProviderForWindow to find the matching WebView Automation Provider."（[ICoreWebView2Environment4](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2environment4#getautomationproviderforwindow)）。visual hosting 不能用 `HWND_MESSAGE` 作父窗口，原因正是 "The underlying implementation of supporting HWND_MESSAGE would break accessibility for visual hosting"（[CreateCoreWebView2CompositionController](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2environment3#createcorewebview2compositioncontroller)）；`NotifyParentWindowPositionChanged` 也被说明是无障碍所需（[NotifyParentWindowPositionChanged](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2controller#notifyparentwindowpositionchanged)）。我读到的 WebView2 文档都没有提到 `WM_GETOBJECT`，WebView2Samples 仓库里也搜不到 `get_AutomationProvider`、`GetAutomationProviderForWindow`、`WM_GETOBJECT` 或 `UiaReturnRawElementProvider` 的用法（GitHub 代码搜索）。因此，“在 `WM_GETOBJECT` 里用 `UiaReturnRawElementProvider` 返回宿主的 provider，再把 WebView 的 provider 作为子节点暴露”只是按 UIA 通用做法的推断，没有官方示例可以对照。

---

## 与题设方案的出入

1. **点击时调 `MoveFocus`：不是必需的。** 微软成员说明 WebView2 收到按下事件时就会自己取得焦点（[#1054](https://github.com/MicrosoftEdge/WebView2Feedback/issues/1054)）。如果保留，建议放在转发按下事件之前（见 2a）。
2. **“窗口从不隐藏，只做 cloak”。** cloak 不会自动让页面变 hidden 或节流（见 3a、3b）。要省资源，必须显式设 `IsVisible=false`（以及可选的 `TrySuspend`）；要求瞬间显示，就保持 `IsVisible=true`，代价是后台持续渲染。另外 #5668 提示可能存在不随宿主一起 cloak 的 WebView2 自有窗口（见 3c）。
3. **“传 `AdditionalBrowserArguments` 会替换嵌入库的默认值”：需要补充。** 对 wry 自己的创建路径和 `put_AdditionalBrowserArguments` 的重复调用来说确实如此；但环境变量和注册表里的参数是追加的，`--enable-features`/`--disable-features` 还会和 WebView2 内置的特性做并集合并。而且 wry 0.57.0 本身没有 composition 路径，自建环境时 wry 的默认值本来就不存在（见 3b）。
4. **复用同一个 visual：没有文档。** 微软示例每次都重建设备、target 和 visual（见 1b）。
5. **`SendPointerInput`：示例依赖实验接口。** 示例里的换算函数来自预发布 SDK，本机的 `webview2-com-sys 0.39.1` 没有绑定它，需要手工填 `ICoreWebView2PointerInfo`（见 1d）。
6. **只用 `SystemCursorId` 会丢掉 CSS 自定义光标**，它在这种情况下返回 0（见 1e）。
7. **`NotifyParentWindowPositionChanged` 只覆盖 HWND 移动**，不覆盖动画容器的偏移；IME 候选窗和弹层位置要靠 `Bounds` 与 visual 位置保持一致（见 2b）。
8. **152+ 运行时的 composition 输入回归**会直接影响“`SendMouseInput` + `SetCapture`”这套转发，目前没有修复版本（见 1d）。

## 待解问题 / 文档未覆盖

1. `ICoreWebView2Controller::Close()` 会不会把 WebView2 挂到 `RootVisualTarget` 下的子 visual 摘掉；同一个 `IDCompositionVisual` 能否给后续新建的 composition controller 再当 `RootVisualTarget`。
2. visual hosting 下 WebView2 是否创建自己的焦点/IME 窗口；它的类名、所属进程、父窗口或所有者是谁；cloak 宿主时它是否继承 cloak。
3. visual hosting 下 WebView2 用什么计算 IME 候选窗、`<select>` 弹层、右键菜单和无障碍矩形的屏幕位置；祖先 visual 的偏移和变换是否会被考虑（目前没有对应 API）。
4. WebView2 是否使用 Chromium 的 `NativeWindowOcclusionTrackerWin`；cloak 宿主窗口在 visual hosting 下是否影响页面可见性或节流；`--disable-features=CalculateNativeWinOcclusion` 等 Chromium 开关在 WebView2 中是否生效。
5. `IVirtualDesktopManager::IsWindowOnCurrentVirtualDesktop` 对 `DWM_CLOAKED_APP` 的窗口返回什么。
6. 通用 Win32 下被 DWM cloak 的窗口是否参与命中测试、能否接收鼠标（只有 RAIL 协议规范写了 MUST NOT）；跨进程能否 cloak 别人的窗口；`DWMWA_TRANSITIONS_FORCEDISABLED` 与 cloak 的交互。
7. `WS_EX_NOREDIRECTIONBITMAP` 能否在创建后切换。
8. composition controller 上 `ShouldDetectMonitorScaleChanges` 和 `BoundsMode` 的默认值。
9. `ICoreWebView2PointerInfo` 的最小必填字段集合。
10. visual hosting 下 `WM_GETOBJECT` 应如何接入 `AutomationProvider`（没有官方示例）。
11. `CreateTargetForHwnd` 是否要求与窗口同线程；释放 target 后是否需要 `Commit` 才能在同一层重建。
12. 152+ composition 输入回归（#5712、#5695）的修复版本；#2758（composition 模式 IME 失效）和 #5570（RDP 下候选窗错位）是否已修复。

## 实测补充（2026-11-01）

本目录的示例程序（`src/main.rs`）在 Tauri 2.12.5、WebView2 运行时 156.0.4314.58、Windows 11 单屏 3840×2160（150%）、微软拼音上测得：输入、输入法和拖出用的是 `Phyllis-x/wry@bd2d093`，其余场景在加固后的 `ff9a717` 上又跑过一遍。只代表这一个运行时版本，不是文档承诺。

- **待解问题 1**：关闭 webview 后提交宿主设备，旧页面的最后一帧会从屏幕上消失（`closevisual` 场景）。`rebuild` 场景重建 60 次，其中 30 次让新 webview 复用上一个 webview 的 visual，另外 30 次换新 visual，每次都正常显示。
- **待解问题 2**：visual hosting 下宿主窗口下有一个 `Chrome_WidgetWin_0` 子窗口，属于宿主进程；点击后它持有键盘焦点（`GetGUIThreadInfo`）。
- **待解问题 4**：宿主窗口 cloak 10 秒期间，页面的 `requestAnimationFrame` 照常运行（约每秒 161 帧），`document.visibilityState` 一直是 `visible`；解除 cloak 后约 6 ms 画面上屏，约 12 ms 出下一帧（`longcloak` 场景）。
- **待解问题 12**：按住左键拖出窗口 5 次后，悬停、点击、键盘仍然到达页面，没有复现 #5712（`dragout` 场景）。从已选中的文字上开始拖时，页面发起 HTML 拖放，OLE 的 `CLIPBRDWNDCLASS` 窗口接管捕获，松开不再以 `mouseup` 送到页面；这是浏览器的正常行为，拖放结束后输入照常。
- **2b（候选窗位置）**：组字起点正下方的 UI Automation 元素属于 `TextInputHost.exe`，名字就是第一候选，与页面算出的组字起点偏差在 10 px 以内（`ime` 场景）。祖先 visual 有偏移时的情况没有测。
