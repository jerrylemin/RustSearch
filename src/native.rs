use crate::{Action, CHROME_HOST, CHROME_ORIGIN, Event, Panel, Result, model::allowed};
use accesskit::{Node, NodeId, Role, TreeId, TreeInfo, TreeUpdate};
use accesskit_windows::SubclassingAdapter;
use euclid::Scale;
use http::{HeaderValue, Method, StatusCode, header};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use servo::{
    Code, CompositionEvent, CompositionState, CreateNewWebViewRequest, DevicePoint,
    EmbedderControl, EventLoopWaker, ImeEvent, InputEvent, JSValue, JavaScriptEvaluationError, Key,
    KeyState, KeyboardEvent, LoadStatus, Location, Modifiers, MouseButton, MouseButtonAction,
    MouseButtonEvent, MouseMoveEvent, NavigationRequest, Opts, Servo, ServoBuilder,
    WebResourceLoad, WebResourceResponse, WebViewBuilder, WebViewDelegate, WheelDelta, WheelEvent,
    WheelMode, WindowRenderingContext,
};
use std::{
    cell::{Cell, RefCell},
    io,
    path::PathBuf,
    rc::{Rc, Weak},
    time::{Duration, Instant},
};
use tao::{
    dpi::{LogicalSize, PhysicalSize},
    event::{ElementState, KeyEvent, MouseScrollDelta, WindowEvent},
    event_loop::{EventLoopProxy, EventLoopWindowTarget},
    keyboard::{Key as TaoKey, KeyLocation, ModifiersState},
    platform::windows::{WindowBuilderExtWindows, WindowExtWindows},
    window::{CursorIcon, Window, WindowBuilder, WindowId},
};
use url::Url;
use windows_sys::Win32::UI::{
    Input::KeyboardAndMouse::SetFocus,
    WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, HWND_BOTTOM, MF_CHECKED, MF_GRAYED, MF_STRING,
        SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos, TPM_NONOTIFY, TPM_RETURNCMD, TrackPopupMenu,
    },
};

#[derive(Clone)]
struct Waker(EventLoopProxy<Event>);

struct AccessibilityActions {
    tab: Option<u32>,
    proxy: EventLoopProxy<Event>,
}

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        let _ = self.0.send_event(Event::ServoWake);
    }
}

pub fn engine(proxy: EventLoopProxy<Event>, config_dir: Option<PathBuf>) -> Result<Servo> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| "Could not initialize Servo TLS")?;
    let opts = Opts {
        temporary_storage: config_dir.is_none(),
        config_dir,
        ..Opts::default()
    };
    let engine = ServoBuilder::default()
        .opts(opts)
        .preferences(servo::Preferences {
            accessibility_enabled: true,
            ..Default::default()
        })
        .event_loop_waker(Box::new(Waker(proxy)))
        .build();
    engine.setup_logging();
    Ok(engine)
}

struct Delegate {
    tab: Option<u32>,
    proxy: EventLoopProxy<Event>,
    window: Rc<Window>,
    exceptions: Rc<RefCell<Vec<String>>>,
    accessibility: RefCell<Weak<RefCell<SubclassingAdapter>>>,
    accessibility_focus: Cell<bool>,
    gesture: Cell<Option<Instant>>,
    popup: RefCell<Option<CreateNewWebViewRequest>>,
    select: RefCell<Option<servo::SelectElement>>,
}

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, _: servo::WebView) {
        self.window.request_redraw();
    }

    fn notify_accessibility_tree_update(
        &self,
        view: servo::WebView,
        update: servo::accesskit::TreeUpdate,
    ) {
        if let Some(adapter) = self.accessibility.borrow().upgrade() {
            let first_webview_tree =
                view.accesskit_tree_id() == Some(update.tree_id) && update.tree.is_some();
            // Servo 0.8 and the Windows adapter use different AccessKit versions.
            let Ok(update) = serde_json::to_value(update).and_then(serde_json::from_value) else {
                return;
            };
            let events = adapter.borrow_mut().update_if_active(|| update);
            if let Some(events) = events {
                events.raise();
            }
            if first_webview_tree && !self.accessibility_focus.replace(true) {
                let events = adapter.borrow_mut().update_if_active(|| TreeUpdate {
                    nodes: Vec::new(),
                    tree: None,
                    tree_id: TreeId::ROOT,
                    focus: NodeId(1),
                });
                if let Some(events) = events {
                    events.raise();
                }
            }
        }
    }

    fn notify_cursor_changed(&self, _: servo::WebView, cursor: servo::Cursor) {
        use servo::Cursor;
        self.window.set_cursor_icon(match cursor {
            Cursor::Pointer => CursorIcon::Hand,
            Cursor::Text => CursorIcon::Text,
            Cursor::VerticalText => CursorIcon::VerticalText,
            Cursor::Crosshair => CursorIcon::Crosshair,
            Cursor::Move => CursorIcon::Move,
            Cursor::Wait => CursorIcon::Wait,
            Cursor::Progress => CursorIcon::Progress,
            Cursor::NotAllowed | Cursor::NoDrop => CursorIcon::NotAllowed,
            Cursor::Grab => CursorIcon::Grab,
            Cursor::Grabbing => CursorIcon::Grabbing,
            _ => CursorIcon::Default,
        });
    }

    fn show_embedder_control(&self, _: servo::WebView, control: EmbedderControl) {
        match control {
            EmbedderControl::InputMethod(ime) => {
                let position = ime.position();
                self.window
                    .set_ime_position(tao::dpi::PhysicalPosition::new(
                        position.min.x,
                        position.max.y,
                    ));
            }
            EmbedderControl::SelectElement(select) if !select.allow_select_multiple() => {
                self.select.replace(Some(select));
                let _ = self.proxy.send_event(Event::SelectOption(self.tab));
            }
            _ => {}
        }
    }

    fn notify_url_changed(&self, _: servo::WebView, _: Url) {
        if let Some(id) = self.tab {
            let _ = self.proxy.send_event(Event::Source(id));
        }
    }

    fn notify_page_title_changed(&self, _: servo::WebView, title: Option<String>) {
        if let Some(id) = self.tab {
            let _ = self
                .proxy
                .send_event(Event::Title(id, title.unwrap_or_default()));
        }
    }

    fn notify_history_changed(&self, _: servo::WebView, _: Vec<Url>, _: usize) {
        if let Some(id) = self.tab {
            let _ = self.proxy.send_event(Event::Source(id));
        }
    }

    fn notify_load_status_changed(&self, _: servo::WebView, status: LoadStatus) {
        if let Some(id) = self.tab
            && status == LoadStatus::Complete
        {
            let _ = self.proxy.send_event(Event::Visited(id));
        }
    }

    fn request_create_new(&self, _: servo::WebView, request: CreateNewWebViewRequest) {
        if let Some(id) = self.tab
            && self
                .gesture
                .take()
                .is_some_and(|time| time.elapsed() < Duration::from_secs(1))
            && self.popup.borrow().is_none()
        {
            self.popup.replace(Some(request));
            let _ = self.proxy.send_event(Event::Popup(id));
        }
    }

    fn notify_closed(&self, _: servo::WebView) {
        if let Some(id) = self.tab {
            let _ = self.proxy.send_event(Event::Action(Action::Close { id }));
        }
    }

    fn request_navigation(&self, _: servo::WebView, request: NavigationRequest) {
        let permitted = if self.tab.is_some() {
            allowed_page(&request.url)
        } else {
            request.url.as_str() == CHROME_ORIGIN
        };
        if !permitted {
            request.deny();
        }
    }

    fn load_web_resource(&self, view: servo::WebView, load: WebResourceLoad) {
        let request = load.request();
        if self.tab.is_none() {
            let (status, body) = self.chrome_request(request);
            let mut response = WebResourceResponse::new(request.url.clone().into_url());
            response.status_code = status;
            response.headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            response
                .headers
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            let mut intercepted = load.intercept(response);
            if !body.is_empty() {
                intercepted.send_body_data(body.to_vec());
            }
            intercepted.finish();
        } else if request.url.host_str() == Some(CHROME_HOST)
            || (!request.is_for_main_frame
                && view.url().is_some_and(|site| {
                    block_tracker(
                        site.as_str(),
                        request.url.as_str(),
                        &self.exceptions.borrow(),
                    )
                }))
        {
            let mut response = WebResourceResponse::new(request.url.clone().into_url());
            response.status_code = StatusCode::FORBIDDEN;
            load.intercept(response).finish();
        }
    }
}

impl Delegate {
    fn chrome_request(&self, request: &servo::WebResourceRequest) -> (StatusCode, &'static [u8]) {
        let url = request.url.as_url();
        if request.method == Method::GET
            && request.is_for_main_frame
            && url.as_str() == CHROME_ORIGIN
        {
            return (StatusCode::OK, include_bytes!("chrome.html"));
        }
        if request.method == Method::GET
            && !request.is_for_main_frame
            && let Some(action) =
                chrome_action(url, request.referrer_url.as_ref().map(|url| url.as_str()))
        {
            let _ = self.proxy.send_event(Event::Action(action));
            return (StatusCode::NO_CONTENT, b"");
        }
        (StatusCode::FORBIDDEN, b"")
    }
}

fn allowed_page(url: &Url) -> bool {
    url.host_str() != Some(CHROME_HOST) && allowed(url.as_str())
}

fn chrome_action(url: &Url, referrer: Option<&str>) -> Option<Action> {
    if referrer != Some(CHROME_ORIGIN)
        || url.origin().ascii_serialization() != CHROME_ORIGIN.trim_end_matches('/')
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/action"
    {
        return None;
    }
    let (_, value) = url.query_pairs().find(|(key, _)| key == "data")?;
    serde_json::from_str(&value).ok()
}

struct ActivateAccessibility(servo::WebView);

impl accesskit::ActivationHandler for ActivateAccessibility {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        let embedded = TreeId(self.0.set_accessibility_active(true)?.0);
        let root = NodeId(0);
        let graft_id = NodeId(1);
        let mut window = Node::new(Role::Window);
        window.set_children(vec![graft_id]);
        let mut graft = Node::new(Role::GenericContainer);
        graft.set_tree_id(embedded);
        let mut tree = TreeInfo::new(root);
        tree.toolkit_name = Some("Servo".into());
        Some(TreeUpdate {
            nodes: vec![(root, window), (graft_id, graft)],
            tree: Some(tree),
            tree_id: TreeId::ROOT,
            focus: root,
        })
    }
}

impl accesskit::ActionHandler for AccessibilityActions {
    fn do_action(&mut self, request: accesskit::ActionRequest) {
        if request.target_tree != TreeId::ROOT
            && let Ok(request) = serde_json::to_value(request).and_then(serde_json::from_value)
        {
            let _ = self
                .proxy
                .send_event(Event::Accessibility(self.tab, request));
        }
    }
}

pub enum ViewSource<'a> {
    Address(&'a str),
    Popup(CreateNewWebViewRequest),
}

pub struct WebView {
    _accessibility: Rc<RefCell<SubclassingAdapter>>,
    view: servo::WebView,
    window: Rc<Window>,
    delegate: Rc<Delegate>,
    visible: Cell<bool>,
    pointer: DevicePoint,
    modifiers: ModifiersState,
    find_text: RefCell<String>,
}

impl WebView {
    pub fn new(
        target: &EventLoopWindowTarget<Event>,
        parent: &Window,
        servo: &Servo,
        proxy: EventLoopProxy<Event>,
        tab: Option<u32>,
        source: ViewSource<'_>,
        exceptions: Rc<RefCell<Vec<String>>>,
    ) -> Result<Self> {
        let window = Rc::new(
            WindowBuilder::new()
                .with_parent_window(parent.hwnd())
                .with_decorations(false)
                .with_visible(false)
                .with_focused(false)
                .with_inner_size(PhysicalSize::new(1, 1))
                .build(target)?,
        );
        let size = window.inner_size();
        let context = Rc::new(
            WindowRenderingContext::new(
                window.display_handle()?,
                window.window_handle()?,
                PhysicalSize::new(size.width.max(1), size.height.max(1)),
            )
            .map_err(|error| format!("Could not create a Servo rendering surface: {error:?}"))?,
        );
        let delegate = Rc::new(Delegate {
            tab,
            proxy: proxy.clone(),
            window: window.clone(),
            exceptions,
            accessibility: RefCell::default(),
            accessibility_focus: Cell::default(),
            gesture: Cell::default(),
            popup: RefCell::default(),
            select: RefCell::default(),
        });
        let builder = match source {
            ViewSource::Address(url) => {
                let url = Url::parse(url)?;
                if tab.is_some() && !allowed_page(&url) {
                    return Err("This address cannot be opened.".into());
                }
                WebViewBuilder::new(servo, context).url(url)
            }
            ViewSource::Popup(request) => request.builder(context),
        };
        let view = builder
            .delegate(delegate.clone())
            .hidpi_scale_factor(Scale::new(parent.scale_factor() as f32))
            .build();
        let accessibility = Rc::new(RefCell::new(SubclassingAdapter::new(
            windows::Win32::Foundation::HWND(window.hwnd() as _),
            ActivateAccessibility(view.clone()),
            AccessibilityActions { tab, proxy },
        )));
        *delegate.accessibility.borrow_mut() = Rc::downgrade(&accessibility);
        Ok(Self {
            _accessibility: accessibility,
            view,
            window,
            delegate,
            visible: Cell::new(false),
            pointer: DevicePoint::zero(),
            modifiers: ModifiersState::empty(),
            find_text: RefCell::default(),
        })
    }

    pub fn take_popup(&self) -> Option<CreateNewWebViewRequest> {
        self.delegate.popup.take()
    }

    pub fn mark_user_gesture(&self) {
        self.delegate.gesture.set(Some(Instant::now()));
    }

    pub fn select_option(&self) {
        let Some(mut select) = self.delegate.select.take() else {
            return;
        };
        let Ok(origin) = self.window.inner_position() else {
            return;
        };
        let position = select.position();
        let selected = select.selected_options();
        use servo::SelectElementOptionOrOptgroup::{Optgroup, Option};
        // SAFETY: The live HWND owns this synchronous menu on the UI thread.
        let choice = unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return;
            }
            let append = |label: &str, id, flags| {
                let label: Vec<_> = label.replace('&', "&&").encode_utf16().chain([0]).collect();
                AppendMenuW(menu, MF_STRING | flags, id, label.as_ptr());
            };
            let append_option = |option: &servo::SelectElementOption| {
                let mut flags = if option.is_disabled { MF_GRAYED } else { 0 };
                if selected.contains(&option.id) {
                    flags |= MF_CHECKED;
                }
                append(&option.label, option.id + 1, flags);
            };
            for item in select.options() {
                match item {
                    Option(option) => append_option(option),
                    Optgroup { label, options } => {
                        if !label.is_empty() {
                            append(label, 0, MF_GRAYED);
                        }
                        for option in options {
                            append_option(option);
                        }
                    }
                }
            }
            let result = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY,
                origin.x + position.min.x,
                origin.y + position.max.y,
                0,
                self.window.hwnd() as _,
                std::ptr::null(),
            );
            DestroyMenu(menu);
            result
        };
        if choice > 0 {
            select.select(vec![choice as usize - 1]);
            select.submit();
        }
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    pub fn url(&self) -> Result<String> {
        Ok(self
            .view
            .url()
            .map(String::from)
            .unwrap_or_else(|| crate::BLANK.into()))
    }

    pub fn load_url(&self, url: &str) -> Result {
        let url = Url::parse(url)?;
        if !allowed_page(&url) {
            return Err("This address cannot be opened.".into());
        }
        self.view.load(url);
        Ok(())
    }

    pub fn set_bounds(&self, x: i32, y: i32, width: i32, height: i32) -> Result {
        let scale = self.window.scale_factor();
        let (x, y) = tao::dpi::LogicalPosition::new(x, y)
            .to_physical::<i32>(scale)
            .into();
        let (width, height) = LogicalSize::new(width.max(1), height.max(1))
            .to_physical::<i32>(scale)
            .into();
        let flags = if self.delegate.tab.is_none() {
            SWP_NOACTIVATE
        } else {
            SWP_NOACTIVATE | SWP_NOZORDER
        };
        // SAFETY: The HWND belongs to this live child window on the UI thread.
        let moved = unsafe {
            SetWindowPos(
                self.window.hwnd() as _,
                HWND_BOTTOM,
                x,
                y,
                width,
                height,
                flags,
            )
        };
        if moved == 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }

    pub fn set_visible(&self, visible: bool) -> Result {
        if self.visible.replace(visible) != visible {
            self.window.set_visible(visible);
            if visible {
                self.view.show();
                self.window.request_redraw();
            } else {
                self.view.hide();
            }
        }
        Ok(())
    }

    pub fn focus(&self) -> Result {
        if self.visible.get() {
            // SAFETY: The HWND belongs to this live child window on the UI thread.
            unsafe { SetFocus(self.window.hwnd() as _) };
            self.view.set_focused(true);
        }
        Ok(())
    }

    pub fn evaluate_script(&self, script: &str) -> Result {
        self.view.evaluate_javascript(script, |_| {});
        Ok(())
    }

    pub fn evaluate_script_with_callback(
        &self,
        script: &str,
        callback: impl FnOnce(std::result::Result<JSValue, JavaScriptEvaluationError>) + 'static,
    ) -> Result {
        self.view.evaluate_javascript(script, callback);
        Ok(())
    }

    pub fn paint(&self) {
        if self.visible.get() {
            self.view.paint();
            self.view.rendering_context().present();
        }
    }

    pub fn window_event(&mut self, event: WindowEvent<'_>) {
        let pressed = |state| state == ElementState::Pressed;
        let input = match event {
            WindowEvent::Resized(size) if size.width != 0 && size.height != 0 => {
                self.view.resize(size);
                return;
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.view
                    .set_hidpi_scale_factor(Scale::new(scale_factor as f32));
                return;
            }
            WindowEvent::Focused(focus) => {
                self.view.set_focused(focus);
                if focus && let Some(id) = self.delegate.tab {
                    let _ = self.delegate.proxy.send_event(Event::Focus(id));
                }
                return;
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers;
                return;
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = DevicePoint::new(position.x as f32, position.y as f32);
                InputEvent::MouseMove(MouseMoveEvent::new(self.pointer.into()))
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let button = match button {
                    tao::event::MouseButton::Left => MouseButton::Primary,
                    tao::event::MouseButton::Right => MouseButton::Secondary,
                    tao::event::MouseButton::Middle => MouseButton::Auxiliary,
                    tao::event::MouseButton::Other(4) => MouseButton::Back,
                    tao::event::MouseButton::Other(5) => MouseButton::Forward,
                    _ => return,
                };
                if pressed(state) && button == MouseButton::Primary {
                    self.mark_user_gesture();
                }
                let action = if pressed(state) {
                    MouseButtonAction::Down
                } else {
                    MouseButtonAction::Up
                };
                InputEvent::MouseButton(MouseButtonEvent::new(action, button, self.pointer.into()))
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (x, y, mode) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        (-f64::from(x), -f64::from(y), WheelMode::DeltaLine)
                    }
                    MouseScrollDelta::PixelDelta(delta) => {
                        (-delta.x, -delta.y, WheelMode::DeltaPixel)
                    }
                    _ => return,
                };
                InputEvent::Wheel(WheelEvent::new(
                    WheelDelta { x, y, z: 0.0, mode },
                    self.pointer.into(),
                ))
            }
            WindowEvent::ReceivedImeText(data) => {
                InputEvent::Ime(ImeEvent::Composition(CompositionEvent {
                    state: CompositionState::End,
                    data,
                }))
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let Some(action) = browser_shortcut(&event, self.modifiers) {
                    if pressed(event.state) && !event.repeat {
                        let _ = self.delegate.proxy.send_event(Event::Action(action));
                    }
                    return;
                }
                if pressed(event.state) && !event.repeat && !self.modifiers.alt_key() {
                    self.mark_user_gesture();
                }
                InputEvent::Keyboard(servo_key(event, self.modifiers))
            }
            _ => return,
        };
        self.view.notify_input_event(input);
    }
}

fn browser_shortcut(event: &KeyEvent, modifiers: ModifiersState) -> Option<Action> {
    let key = match event.key_without_modifiers() {
        TaoKey::ArrowLeft => 37,
        TaoKey::ArrowRight => 39,
        TaoKey::Tab => 9,
        TaoKey::Escape => 27,
        TaoKey::F5 => 116,
        TaoKey::Character(key) if key.len() == 1 => {
            u32::from(key.as_bytes()[0].to_ascii_uppercase())
        }
        _ => return None,
    };
    shortcut(
        key,
        modifiers.control_key(),
        modifiers.shift_key(),
        modifiers.alt_key(),
    )
}

fn servo_key(event: KeyEvent, modifiers: ModifiersState) -> KeyboardEvent {
    let key = match event.logical_key {
        TaoKey::Character(text) => Key::Character(text.into()),
        key => format!("{key:?}")
            .parse()
            .unwrap_or(Key::Named(servo::NamedKey::Unidentified)),
    };
    let code = format!("{:?}", event.physical_key)
        .parse()
        .unwrap_or(Code::Unidentified);
    let location = match event.location {
        KeyLocation::Left => Location::Left,
        KeyLocation::Right => Location::Right,
        KeyLocation::Numpad => Location::Numpad,
        _ => Location::Standard,
    };
    let state = if event.state == ElementState::Pressed {
        KeyState::Down
    } else {
        KeyState::Up
    };
    let mut flags = Modifiers::empty();
    flags.set(Modifiers::CONTROL, modifiers.control_key());
    flags.set(Modifiers::SHIFT, modifiers.shift_key());
    flags.set(Modifiers::ALT, modifiers.alt_key());
    flags.set(Modifiers::META, modifiers.super_key());
    KeyboardEvent::new_without_event(state, key, code, location, flags, event.repeat, false)
}

pub fn navigation(view: &WebView, action: Action) -> Result {
    match action {
        Action::Back if view.view.can_go_back() => {
            view.view.go_back(1);
        }
        Action::Forward if view.view.can_go_forward() => {
            view.view.go_forward(1);
        }
        Action::Reload => view.view.reload(),
        _ => {}
    }
    Ok(())
}

pub fn directions(view: &WebView) -> (bool, bool) {
    (view.view.can_go_back(), view.view.can_go_forward())
}

pub fn find(view: &WebView, action: Action) -> Result {
    let backwards = matches!(action, Action::FindPrevious);
    if let Action::FindText { text } = action {
        view.find_text.replace(text);
    } else if matches!(action, Action::CloseFind) {
        view.find_text.borrow_mut().clear();
    }
    let text = view.find_text.borrow();
    view.evaluate_script(&format!(
        "{}({}, {backwards})",
        include_str!("find.js"),
        serde_json::json!(&*text)
    ))
}

pub fn picture(_: &WebView, proxy: &EventLoopProxy<Event>) -> Result {
    let _ = proxy.send_event(Event::Message(
        "Picture in picture is unavailable in Servo.".into(),
    ));
    Ok(())
}

const TRACKERS: &[&str] = &[
    "doubleclick.net",
    "googlesyndication.com",
    "googleadservices.com",
    "googletagservices.com",
    "google-analytics.com",
    "googletagmanager.com",
    "adservice.google.com",
    "amazon-adsystem.com",
    "adnxs.com",
    "adsrvr.org",
    "criteo.com",
    "criteo.net",
    "taboola.com",
    "outbrain.com",
    "rubiconproject.com",
    "pubmatic.com",
    "openx.net",
    "casalemedia.com",
    "smartadserver.com",
    "sharethrough.com",
    "indexww.com",
    "bidswitch.net",
    "33across.com",
    "teads.tv",
    "moatads.com",
    "adroll.com",
    "scorecardresearch.com",
    "quantserve.com",
    "chartbeat.com",
    "hotjar.com",
    "mouseflow.com",
    "fullstory.com",
    "clarity.ms",
    "mixpanel.com",
    "amplitude.com",
    "segment.com",
    "segment.io",
    "branch.io",
    "appsflyer.com",
    "adjust.com",
    "analytics.tiktok.com",
    "connect.facebook.net",
    "ads-twitter.com",
    "analytics.twitter.com",
];

fn matches_domain(host: &str, domain: &str) -> bool {
    host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

fn block_tracker(site: &str, resource: &str, exceptions: &[String]) -> bool {
    let (Ok(site), Ok(resource)) = (url::Url::parse(site), url::Url::parse(resource)) else {
        return false;
    };
    let (Some(site), Some(resource)) = (site.host_str(), resource.host_str()) else {
        return false;
    };
    !matches_domain(resource, site)
        && !exceptions.iter().any(|host| host == site)
        && TRACKERS
            .iter()
            .any(|domain| matches_domain(resource, domain) && !matches_domain(site, domain))
}

fn shortcut(key: u32, ctrl: bool, shift: bool, alt: bool) -> Option<Action> {
    if alt && !ctrl && !shift {
        return match key {
            37 => Some(Action::Back),
            39 => Some(Action::Forward),
            _ => None,
        };
    }
    if ctrl && !alt {
        return match (shift, key) {
            (false, 0x4C) => Some(Action::Address),
            (false, 0x54) => Some(Action::NewTab),
            (false, 0x57) => Some(Action::CloseActive),
            (false, 0x52) => Some(Action::Reload),
            (false, 0x44) => Some(Action::Bookmark),
            (false, 0x46) => Some(Action::OpenFind),
            (false, 0x48) => Some(Action::OpenPanel {
                panel: Panel::History,
            }),
            (false, 0x4A) => Some(Action::OpenPanel {
                panel: Panel::Downloads,
            }),
            (false, 0x4B) => Some(Action::OpenPanel { panel: Panel::Tabs }),
            (false, 0x31..=0x39) => Some(Action::Jump {
                index: (key - 0x31) as usize,
            }),
            (false, 9) => Some(Action::Cycle { reverse: false }),
            (true, 9) => Some(Action::Cycle { reverse: true }),
            (true, 0x54) => Some(Action::Reopen),
            (true, 0x4E) => Some(Action::PrivateTab),
            (true, 0x52) => Some(Action::Reader),
            (true, 0x50) => Some(Action::Picture),
            (true, 0x42) => Some(Action::OpenPanel {
                panel: Panel::Bookmarks,
            }),
            (true, 0x53) => Some(Action::Layout),
            _ => None,
        };
    }
    match key {
        27 => Some(Action::Dismiss),
        116 => Some(Action::Reload),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, CHROME_ORIGIN, Url, block_tracker, chrome_action};

    #[test]
    fn chrome_actions_require_the_local_origin_and_referrer() {
        let path = "action?data=%7B%22action%22%3A%22new_tab%22%7D";
        let local = Url::parse(&format!("{CHROME_ORIGIN}{path}")).unwrap();
        let foreign = Url::parse(&format!("https://example.com/{path}")).unwrap();
        assert!(matches!(
            chrome_action(&local, Some(CHROME_ORIGIN)),
            Some(Action::NewTab)
        ));
        assert!(chrome_action(&local, Some("https://example.com/")).is_none());
        assert!(chrome_action(&local, None).is_none());
        assert!(chrome_action(&foreign, Some(CHROME_ORIGIN)).is_none());
    }

    #[test]
    fn filters_only_third_party_tracker_domains() {
        let site = "https://example.com/";
        assert!(block_tracker(site, "https://cdn.doubleclick.net/ad", &[]));
        assert!(!block_tracker(site, "https://fakedoubleclick.net/ad", &[]));
        assert!(!block_tracker(
            site,
            "https://doubleclick.net.example.com/ad",
            &[]
        ));
        assert!(!block_tracker(
            site,
            "https://doubleclick.net/ad",
            &["example.com".into()]
        ));
        assert!(!block_tracker(
            "https://doubleclick.net/",
            "https://cdn.doubleclick.net/ad",
            &[]
        ));
    }
}
