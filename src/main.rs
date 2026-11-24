#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod model;
mod native;

use native::{ViewSource, WebView};

use model::{BLANK, Engine, Page, SavedTab, State, address, allowed, web};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{cell::RefCell, env, error::Error, fs, io, path::PathBuf, rc::Rc};
use tao::{
    dpi::LogicalSize,
    event::{Event as TaoEvent, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget},
    platform::run_return::EventLoopExtRunReturn,
    window::{Window, WindowBuilder},
};
use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MessageBoxW};

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;
const CHROME_ORIGIN: &str = "https://rustsearch.ui.invalid/";
const CHROME_HOST: &str = "rustsearch.ui.invalid";

#[derive(Clone, Copy, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum Panel {
    Menu,
    Tabs,
    History,
    Bookmarks,
    Downloads,
    Settings,
    Split,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Action {
    Ready,
    Navigate { text: String },
    NewTab,
    PrivateTab,
    Select { id: u32 },
    Close { id: u32 },
    CloseActive,
    Reopen,
    Pin,
    ToggleTracker,
    Split,
    ResizeSplit { percent: u8 },
    PickElement,
    KeepHidden,
    CancelPick,
    ResetHidden,
    Bookmark,
    Back,
    Forward,
    Reload,
    Address,
    OpenFind,
    FindText { text: String },
    FindNext,
    FindPrevious,
    CloseFind,
    Reader,
    Picture,
    OpenPanel { panel: Panel },
    ClosePanel,
    OpenItem { id: usize },
    RemoveItem { id: usize },
    Layout,
    Engine { engine: Engine },
    Jump { index: usize },
    Cycle { reverse: bool },
    Dismiss,
}

enum Event {
    Action(Action),
    Source(u32),
    Visited(u32),
    Title(u32, String),
    Focus(u32),
    Popup(u32),
    SelectOption(Option<u32>),
    ServoWake,
    Accessibility(Option<u32>, servo::accesskit::ActionRequest),
    Message(String),
    HiddenChoice(u32, String, Option<String>),
}

struct Tab {
    id: u32,
    saved: SavedTab,
    private: bool,
    view: Option<WebView>,
}

#[derive(Serialize)]
struct ChromeTab<'a> {
    id: u32,
    label: &'a str,
    pinned: bool,
    private: bool,
}

#[derive(Serialize)]
struct ChromeItem<'a> {
    id: usize,
    label: &'a str,
    detail: &'a str,
}

#[derive(Serialize)]
struct ChromeState<'a> {
    active: u32,
    private: bool,
    pinned: bool,
    tabs: Vec<ChromeTab<'a>>,
    address: &'a str,
    back: bool,
    forward: bool,
    find: bool,
    suggestions: &'a [Page],
    panel: Option<Panel>,
    items: Vec<ChromeItem<'a>>,
    split: bool,
    split_percent: u8,
    picking: bool,
    can_hide: bool,
    has_hidden: bool,
    bookmarked: bool,
    can_bookmark: bool,
    engine: Engine,
    side_tabs: bool,
    tracker_toggle: bool,
    tracker_disabled: bool,
}

struct Browser {
    chrome: WebView,
    proxy: EventLoopProxy<Event>,
    tabs: Vec<Tab>,
    active: u32,
    split: Option<[u32; 2]>,
    next_id: u32,
    closed: Vec<SavedTab>,
    panel: Option<Panel>,
    find: bool,
    ready: bool,
    state: State,
    tracker_exceptions: Rc<RefCell<Vec<String>>>,
    picking: Option<(u32, String)>,
    state_path: PathBuf,
    private_window: bool,
    servo: servo::Servo,
    _profile_lock: Option<fs::File>,
    window: Window,
}

impl Browser {
    fn index(&self, id: u32) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    fn active(&self) -> &Tab {
        &self.tabs[self
            .index(self.active)
            .expect("active tab is in the tab list")]
    }

    fn add(&mut self, saved: SavedTab, private: bool) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(Tab {
            id,
            saved,
            private,
            view: None,
        });
        id
    }

    fn blank(&mut self, private: bool) -> u32 {
        self.add(
            SavedTab {
                page: Page {
                    url: BLANK.into(),
                    title: String::new(),
                },
                pinned: false,
            },
            private,
        )
    }

    fn ensure_view(
        &mut self,
        target: &EventLoopWindowTarget<Event>,
        id: u32,
        url: Option<&str>,
    ) -> Result {
        let Some(index) = self.index(id) else {
            return Ok(());
        };
        let tab = &mut self.tabs[index];
        if tab.view.is_none() {
            tab.view = Some(WebView::new(
                target,
                &self.window,
                &self.servo,
                self.proxy.clone(),
                Some(id),
                ViewSource::Address(url.unwrap_or(&tab.saved.page.url)),
                self.tracker_exceptions.clone(),
            )?);
        }
        Ok(())
    }

    fn select(&mut self, target: &EventLoopWindowTarget<Event>, id: u32, focus: bool) -> Result {
        if self.index(id).is_none() {
            return Ok(());
        }
        self.end_find();
        self.cancel_pick();
        self.active = id;
        self.panel = None;
        let pair = self.split.filter(|pair| pair.contains(&id));
        if let Some([left, right]) = pair {
            self.ensure_view(target, left, None)?;
            self.ensure_view(target, right, None)?;
        } else if self.active().saved.page.url != BLANK {
            self.ensure_view(target, id, None)?;
        }
        self.layout()?;
        if focus {
            if let Some(view) = &self.active().view {
                view.focus()?;
            } else {
                self.focus_address()?;
            }
        }
        Ok(())
    }

    fn close(&mut self, target: &EventLoopWindowTarget<Event>, id: u32) -> Result {
        let Some(index) = self.index(id) else {
            return Ok(());
        };
        let tab = self.tabs.remove(index);
        let other = self
            .split
            .filter(|pair| pair.contains(&id))
            .map(|[left, right]| if left == id { right } else { left });
        if other.is_some() {
            self.split = None;
        }
        if !tab.private && tab.saved.page.url != BLANK {
            self.closed.push(tab.saved);
            if self.closed.len() > 25 {
                self.closed.remove(0);
            }
        }
        if self.active == id {
            self.find = false;
            let next = if let Some(other) = other {
                other
            } else if self.tabs.is_empty() {
                self.blank(self.private_window)
            } else {
                self.tabs[index.min(self.tabs.len() - 1)].id
            };
            self.select(target, next, true)?;
        }
        self.layout()
    }

    fn navigate(&mut self, target: &EventLoopWindowTarget<Event>, url: &str) -> Result {
        self.end_find();
        self.cancel_pick();
        self.panel = None;
        if let Some(view) = self.active().view.as_ref() {
            view.load_url(url)?;
        } else {
            self.ensure_view(target, self.active, Some(url))?;
        }
        self.layout()?;
        self.active()
            .view
            .as_ref()
            .expect("active view is still present")
            .focus()?;
        Ok(())
    }

    fn layout(&mut self) -> Result {
        let size = self
            .window
            .inner_size()
            .to_logical::<i32>(self.window.scale_factor());
        self.chrome.set_bounds(0, 0, size.width, size.height)?;
        let x = if self.state.side_tabs { 214 } else { 0 };
        let y = if self.state.side_tabs { 52 } else { 92 } + if self.find { 38 } else { 0 };
        let width = size.width - x;
        let height = size.height - y;
        let pair = self.split.filter(|pair| pair.contains(&self.active));
        let left_width = (width - 6) * i32::from(self.split_percent()) / 100;
        for tab in &mut self.tabs {
            let Some(view) = &tab.view else { continue };
            let foreground =
                tab.id == self.active || pair.is_some_and(|pair| pair.contains(&tab.id));
            let showing = self.panel.is_none() && foreground;
            if showing {
                let (x, y, width, height) = match pair {
                    Some([left, _]) if left == tab.id => (x, y, left_width, height),
                    Some(_) => {
                        let left = left_width + 6;
                        (x + left, y, width - left, height)
                    }
                    None => (x, y, width, height),
                };
                view.set_bounds(x, y, width, height)?;
            }
            view.set_visible(showing)?;
        }
        Ok(())
    }

    fn focus_address(&self) -> Result {
        self.chrome.focus()?;
        self.chrome.evaluate_script("focusAddress()")?;
        Ok(())
    }

    fn split_percent(&self) -> u8 {
        self.state.split_percent.unwrap_or(50).clamp(25, 75)
    }

    fn host(tab: &Tab) -> Option<String> {
        if tab.private || !web(&tab.saved.page.url) {
            return None;
        }
        url::Url::parse(&tab.saved.page.url)
            .ok()?
            .host_str()
            .map(str::to_owned)
    }

    fn apply_hidden(&self, tab: &Tab) -> Result {
        let Some((view, host)) = tab.view.as_ref().zip(Self::host(tab)) else {
            return Ok(());
        };
        let selectors = self
            .state
            .hidden_elements
            .get(&host)
            .map(Vec::as_slice)
            .unwrap_or_default();
        view.evaluate_script(&format!(r#"(() => {{
            if (location.hostname !== {host}) return;
            const key = '__rustsearchHiddenSheet';
            document.adoptedStyleSheets = document.adoptedStyleSheets.filter(sheet => sheet !== window[key]);
            const sheet = new CSSStyleSheet();
            for (const selector of {selectors}) {{
                try {{ document.querySelector(selector); sheet.insertRule(selector + '{{display:none!important}}'); }} catch {{}}
            }}
            document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
            window[key] = sheet;
        }})()"#, host = json!(host), selectors = json!(selectors)))?;
        Ok(())
    }

    fn cancel_pick(&mut self) {
        if let Some((id, _)) = self.picking.take()
            && let Some(view) = self
                .index(id)
                .and_then(|index| self.tabs[index].view.as_ref())
        {
            let _ = view.evaluate_script("window.__rustsearchPick?.dispose()");
        }
    }

    fn end_find(&mut self) {
        if self.find {
            if let Some(view) = &self.active().view {
                let _ = native::find(view, Action::CloseFind);
            }
            self.find = false;
        }
    }

    fn items(&self) -> Vec<ChromeItem<'_>> {
        match self.panel {
            Some(Panel::History | Panel::Bookmarks) => {
                let pages = if matches!(self.panel, Some(Panel::History)) {
                    &self.state.history
                } else {
                    &self.state.bookmarks
                };
                pages
                    .iter()
                    .enumerate()
                    .map(|(id, page)| ChromeItem {
                        id,
                        label: page.label(),
                        detail: &page.url,
                    })
                    .collect()
            }
            Some(Panel::Tabs | Panel::Split) => self
                .tabs
                .iter()
                .filter(|tab| !matches!(self.panel, Some(Panel::Split)) || tab.id != self.active)
                .map(|tab| ChromeItem {
                    id: tab.id as usize,
                    label: tab.saved.page.label(),
                    detail: &tab.saved.page.url,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    fn render(&self) -> Result {
        if !self.ready {
            return Ok(());
        }
        let tab = self.active();
        let page = &tab.saved.page;
        let current_host = Self::host(tab);
        let host = current_host.as_deref();
        let (back, forward) = tab
            .view
            .as_ref()
            .map(native::directions)
            .unwrap_or_default();
        let tabs = self
            .tabs
            .iter()
            .map(|tab| ChromeTab {
                id: tab.id,
                label: tab.saved.page.label(),
                pinned: tab.saved.pinned,
                private: tab.private,
            })
            .collect();
        let suggestions = if tab.private {
            &[]
        } else {
            &self.state.history[..self.state.history.len().min(100)]
        };
        let data = ChromeState {
            active: tab.id,
            private: tab.private,
            pinned: tab.saved.pinned,
            tabs,
            address: if page.url == BLANK { "" } else { &page.url },
            back,
            forward,
            find: self.find,
            suggestions,
            panel: self.panel,
            items: self.items(),
            split: self.split.is_some_and(|pair| pair.contains(&tab.id)),
            split_percent: self.split_percent(),
            picking: self.picking.is_some(),
            can_hide: host.is_some(),
            has_hidden: host.is_some_and(|host| self.state.hidden_elements.contains_key(host)),
            bookmarked: !tab.private
                && self.state.bookmarks.iter().any(|item| item.url == page.url),
            can_bookmark: !tab.private && web(&page.url),
            engine: self.state.engine,
            side_tabs: self.state.side_tabs,
            tracker_toggle: !tab.private && host.is_some(),
            tracker_disabled: host.is_some_and(|host| {
                self.state
                    .tracker_exceptions
                    .iter()
                    .any(|site| site == host)
            }),
        };
        self.window.set_title(&format!(
            "{} · RustSearch",
            if tab.private {
                "Private tab"
            } else {
                page.label()
            }
        ));
        let mut script = b"render(".to_vec();
        serde_json::to_writer(&mut script, &data)?;
        script.push(b')');
        self.chrome.evaluate_script(std::str::from_utf8(&script)?)?;
        Ok(())
    }

    fn persist(&mut self) -> Result {
        if self.private_window {
            return Ok(());
        }
        let regular = self.tabs.iter().filter(|tab| !tab.private);
        let position = |id| regular.clone().position(|tab| tab.id == id);
        self.state.selected = position(self.active).unwrap_or_default();
        self.state.split = self
            .split
            .and_then(|[left, right]| Some([position(left)?, position(right)?]));
        self.state
            .write(&self.state_path, regular.map(|tab| &tab.saved))?;
        Ok(())
    }

    fn notify(&self, message: &str) {
        eprintln!("{message}");
        if self.ready {
            let _ = self
                .chrome
                .evaluate_script(&format!("showMessage({})", json!(message)));
        }
    }

    fn action(&mut self, target: &EventLoopWindowTarget<Event>, action: Action) -> Result<bool> {
        let mut save = false;
        match action {
            Action::Ready => {
                self.ready = true;
                if let Some(view) = &self.active().view {
                    view.focus()?;
                } else {
                    self.focus_address()?;
                }
            }
            Action::Navigate { text } => {
                if let Some(url) = address(&text, self.state.engine) {
                    self.navigate(target, &url)?;
                } else if !text.trim().is_empty() {
                    self.notify("This address cannot be opened.");
                }
            }
            Action::PrivateTab if !self.private_window => {
                std::process::Command::new(env::current_exe()?)
                    .arg("--private-window")
                    .spawn()?;
            }
            Action::NewTab | Action::PrivateTab => {
                let id = self.blank(self.private_window);
                self.select(target, id, true)?;
                save = !self.private_window;
            }
            Action::Select { id } => {
                self.select(target, id, true)?;
                save = true;
            }
            Action::Close { id } => {
                self.close(target, id)?;
                save = true;
            }
            Action::CloseActive => {
                self.close(target, self.active)?;
                save = true;
            }
            Action::Jump { index } => {
                if let Some(tab) = self.tabs.get(index) {
                    self.select(target, tab.id, true)?;
                    save = true;
                }
            }
            Action::Cycle { reverse } => {
                let length = self.tabs.len();
                let current = self.index(self.active).unwrap_or_default();
                let next = if reverse {
                    (current + length - 1) % length
                } else {
                    (current + 1) % length
                };
                self.select(target, self.tabs[next].id, true)?;
                save = true;
            }
            Action::Reopen => {
                if let Some(saved) = self.closed.pop() {
                    let id = self.add(saved, false);
                    self.select(target, id, true)?;
                    save = true;
                }
            }
            Action::Pin => {
                let index = self.index(self.active).unwrap_or_default();
                if !self.tabs[index].private {
                    let mut tab = self.tabs.remove(index);
                    tab.saved.pinned = !tab.saved.pinned;
                    let first_regular = self
                        .tabs
                        .iter()
                        .position(|tab| !tab.saved.pinned)
                        .unwrap_or(self.tabs.len());
                    self.tabs.insert(first_regular, tab);
                    save = true;
                }
            }
            Action::Split => {
                if self.split.is_some_and(|pair| pair.contains(&self.active)) {
                    self.split = None;
                    self.select(target, self.active, true)?;
                    save = true;
                } else {
                    if self.tabs.len() == 1 {
                        self.blank(self.active().private);
                    }
                    self.panel = Some(Panel::Split);
                    self.layout()?;
                }
            }
            Action::ResizeSplit { percent } => {
                if self.split.is_some() {
                    self.state.split_percent = Some(percent.clamp(25, 75));
                    self.layout()?;
                    save = true;
                }
            }
            Action::PickElement => {
                self.select(target, self.active, true)?;
                let tab = self.active();
                if let Some((view, site)) = tab.view.as_ref().zip(Self::host(tab)) {
                    view.evaluate_script(include_str!("hide.js"))?;
                    self.picking = Some((tab.id, site));
                    self.notify("Click an element on the page, then choose Keep hidden.");
                }
            }
            Action::KeepHidden => {
                if let Some((id, site)) = self.picking.clone()
                    && let Some(view) = self.active().view.as_ref()
                {
                    let proxy = self.proxy.clone();
                    view.evaluate_script_with_callback(
                        "window.__rustsearchPick?.selector || null",
                        move |result| {
                            let selector = match result {
                                Ok(servo::JSValue::String(selector)) => Some(selector),
                                _ => None,
                            };
                            let _ =
                                proxy.send_event(Event::HiddenChoice(id, site.clone(), selector));
                        },
                    )?;
                }
            }
            Action::CancelPick => self.cancel_pick(),
            Action::ResetHidden => {
                if let Some(site) = Self::host(self.active())
                    && self.state.hidden_elements.remove(&site).is_some()
                {
                    self.select(target, self.active, true)?;
                    for tab in &self.tabs {
                        if Self::host(tab).as_deref() == Some(&site) {
                            self.apply_hidden(tab)?;
                        }
                    }
                    save = true;
                }
            }
            Action::Bookmark => {
                let tab = self.active();
                if !tab.private && web(&tab.saved.page.url) {
                    let page = tab.saved.page.clone();
                    if let Some(index) = self
                        .state
                        .bookmarks
                        .iter()
                        .position(|item| item.url == page.url)
                    {
                        self.state.bookmarks.remove(index);
                    } else {
                        self.state.bookmarks.insert(0, page);
                    }
                    save = true;
                }
            }
            Action::ToggleTracker => {
                if !self.active().private
                    && web(&self.active().saved.page.url)
                    && let Ok(url) = url::Url::parse(&self.active().saved.page.url)
                    && let Some(host) = url.host_str()
                {
                    let sites = &mut self.state.tracker_exceptions;
                    if let Some(index) = sites.iter().position(|site| site == host) {
                        sites.remove(index);
                    } else {
                        sites.push(host.to_owned());
                    }
                    self.tracker_exceptions.borrow_mut().clone_from(sites);
                    self.select(target, self.active, true)?;
                    if let Some(view) = &self.active().view {
                        native::navigation(view, Action::Reload)?;
                    }
                    save = true;
                }
            }
            Action::Back | Action::Forward | Action::Reload => {
                if let Some(view) = &self.active().view {
                    native::navigation(view, action)?;
                }
            }
            Action::Address => self.focus_address()?,
            Action::OpenFind => {
                if self.active().view.is_some() {
                    self.panel = None;
                    self.find = true;
                    self.layout()?;
                    self.chrome.focus()?;
                    self.chrome.evaluate_script("byId('find-row').hidden=false; byId('find').focus(); byId('find').select()")?;
                }
            }
            Action::FindText { .. } | Action::FindNext | Action::FindPrevious => {
                if self.find
                    && let Some(view) = &self.active().view
                {
                    native::find(view, action)?;
                }
            }
            Action::CloseFind => {
                if self.find {
                    self.select(target, self.active, true)?;
                }
            }
            Action::Reader | Action::Picture => {
                self.select(target, self.active, true)?;
                if let Some(view) = &self.active().view {
                    match action {
                        Action::Reader => view.evaluate_script(include_str!("reader.js"))?,
                        Action::Picture => native::picture(view, &self.proxy)?,
                        _ => unreachable!(),
                    }
                }
            }
            Action::OpenPanel {
                panel: Panel::Downloads,
            } => self.notify("Downloads are unavailable in this Servo build."),
            Action::OpenPanel { panel } => {
                self.end_find();
                self.cancel_pick();
                self.panel = Some(panel);
                self.layout()?;
                self.chrome.focus()?;
            }
            Action::ClosePanel | Action::Dismiss if self.panel.is_some() => {
                self.select(target, self.active, true)?
            }
            Action::Dismiss if self.find => {
                self.select(target, self.active, true)?;
            }
            Action::Dismiss if self.picking.is_some() => self.cancel_pick(),
            Action::ClosePanel | Action::Dismiss => {}
            Action::OpenItem { id } => match self.panel {
                Some(Panel::Tabs) => {
                    if let Ok(id) = id.try_into() {
                        self.select(target, id, true)?;
                        save = true;
                    }
                }
                Some(Panel::Split) => {
                    if let Ok(id) = u32::try_from(id)
                        && id != self.active
                        && self.index(id).is_some()
                    {
                        self.split = Some([self.active, id]);
                        self.select(target, self.active, true)?;
                        save = true;
                    }
                }
                Some(Panel::History | Panel::Bookmarks) => {
                    let pages = if matches!(self.panel, Some(Panel::History)) {
                        &self.state.history
                    } else {
                        &self.state.bookmarks
                    };
                    if let Some(page) = pages
                        .get(id)
                        .filter(|page| web(&page.url) && allowed(&page.url))
                    {
                        let url = page.url.clone();
                        self.navigate(target, &url)?;
                    }
                }
                _ => {}
            },
            Action::RemoveItem { id } => {
                let items = match self.panel {
                    Some(Panel::History) => &mut self.state.history,
                    Some(Panel::Bookmarks) => &mut self.state.bookmarks,
                    _ => return Ok(false),
                };
                if id < items.len() {
                    items.remove(id);
                    save = true;
                }
            }
            Action::Layout => {
                self.state.side_tabs = !self.state.side_tabs;
                self.layout()?;
                save = true;
            }
            Action::Engine { engine } => {
                self.state.engine = engine;
                save = true;
            }
        }
        Ok(save)
    }

    fn event(&mut self, target: &EventLoopWindowTarget<Event>, event: Event) -> Result {
        let save = match event {
            Event::Action(action) => self.action(target, action)?,
            Event::Focus(id)
                if id != self.active
                    && self
                        .split
                        .is_some_and(|pair| pair.contains(&id) && pair.contains(&self.active)) =>
            {
                self.end_find();
                self.cancel_pick();
                self.active = id;
                self.layout()?;
                true
            }
            Event::Focus(_) => return Ok(()),
            Event::Title(id, title) => {
                let Some(index) = self.index(id) else {
                    return Ok(());
                };
                let tab = &mut self.tabs[index];
                tab.saved.page.title = title.chars().take(512).collect();
                if !tab.private
                    && let Some(item) = self
                        .state
                        .history
                        .iter_mut()
                        .find(|item| item.url == tab.saved.page.url)
                {
                    item.title.clone_from(&tab.saved.page.title);
                }
                !tab.private
            }
            Event::Source(id) | Event::Visited(id) => {
                let Some(index) = self.index(id) else {
                    return Ok(());
                };
                let tab = &mut self.tabs[index];
                let Some(view) = &tab.view else { return Ok(()) };
                let url = view.url()?;
                if !allowed(&url) {
                    return Ok(());
                }
                let changed = tab.saved.page.url != url;
                if changed {
                    tab.saved.page.url = url;
                    tab.saved.page.title.clear();
                }
                let visit =
                    !tab.private && matches!(event, Event::Visited(_)) && web(&tab.saved.page.url);
                if visit {
                    self.state.visit(&tab.saved.page);
                }
                if changed && self.picking.as_ref().is_some_and(|(tab, _)| *tab == id) {
                    self.cancel_pick();
                }
                if visit {
                    self.apply_hidden(&self.tabs[index])?;
                }
                visit
            }
            Event::Popup(opener) => {
                let Some(request) = self
                    .index(opener)
                    .and_then(|index| self.tabs[index].view.as_ref())
                    .and_then(WebView::take_popup)
                else {
                    return Ok(());
                };
                let id = self.next_id;
                let view = WebView::new(
                    target,
                    &self.window,
                    &self.servo,
                    self.proxy.clone(),
                    Some(id),
                    ViewSource::Popup(request),
                    self.tracker_exceptions.clone(),
                )?;
                let id = self.blank(self.private_window);
                self.tabs.last_mut().expect("blank added a tab").view = Some(view);
                self.select(target, id, true)?;
                !self.private_window
            }
            Event::Accessibility(tab, request) => {
                if request.action == servo::accesskit::Action::Click
                    && let Some(view) = tab
                        .and_then(|id| self.index(id))
                        .and_then(|index| self.tabs[index].view.as_ref())
                {
                    view.mark_user_gesture();
                }
                self.servo.forward_accessibility_action(request);
                return Ok(());
            }
            Event::SelectOption(tab) => {
                let view = match tab {
                    Some(id) => self
                        .index(id)
                        .and_then(|index| self.tabs[index].view.as_ref()),
                    None => Some(&self.chrome),
                };
                if let Some(view) = view {
                    view.select_option();
                }
                return Ok(());
            }
            Event::ServoWake => {
                self.servo.spin_event_loop();
                return Ok(());
            }
            Event::Message(message) => {
                self.notify(&message);
                return Ok(());
            }
            Event::HiddenChoice(id, site, selector) => {
                if !self
                    .picking
                    .as_ref()
                    .is_some_and(|(picking, host)| *picking == id && *host == site)
                {
                    return Ok(());
                }
                let Some(selector) =
                    selector.filter(|selector| !selector.is_empty() && selector.len() <= 4096)
                else {
                    self.notify("Click an element on the page first.");
                    return Ok(());
                };
                self.cancel_pick();
                let selectors = self.state.hidden_elements.entry(site.clone()).or_default();
                if !selectors.contains(&selector) {
                    selectors.push(selector);
                }
                for tab in &self.tabs {
                    if Self::host(tab).as_deref() == Some(&site) {
                        self.apply_hidden(tab)?;
                    }
                }
                true
            }
        };
        self.render()?;
        if save {
            self.persist()?;
        }
        Ok(())
    }
}

fn profile_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os("RUSTSEARCH_DATA_DIR") {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err("RUSTSEARCH_DATA_DIR must be an absolute path".into());
        }
        return Ok(path);
    }
    let local = env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is unavailable")?;
    Ok(PathBuf::from(local).join("RustSearch"))
}

fn run() -> Result {
    let mut args = env::args_os().skip(1);
    let mut initial = args.next();
    let private_window = initial
        .as_deref()
        .is_some_and(|arg| arg == "--private-window");
    if private_window {
        initial = args.next();
    }
    let root = if private_window {
        PathBuf::new()
    } else {
        profile_path()?
    };
    let state_path = root.join("session.json");
    let (lock, state, config_dir) = if private_window {
        (None, State::default(), None)
    } else {
        fs::create_dir_all(&root)?;
        let lock = fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("profile.lock"))?;
        lock.try_lock().map_err(|error| match error {
            fs::TryLockError::WouldBlock => io::Error::new(
                io::ErrorKind::WouldBlock,
                "RustSearch is already open for this profile.",
            ),
            fs::TryLockError::Error(error) => error,
        })?;
        let config_dir = root.join("Servo");
        fs::create_dir_all(&config_dir)?;
        (Some(lock), State::read(&state_path)?, Some(config_dir))
    };
    let tracker_exceptions = Rc::new(RefCell::new(state.tracker_exceptions.clone()));
    let mut events = EventLoopBuilder::<Event>::with_user_event().build();
    let proxy = events.create_proxy();
    let window = WindowBuilder::new()
        .with_title("RustSearch")
        .with_inner_size(LogicalSize::new(1120, 760))
        .with_min_inner_size(LogicalSize::new(530, 360))
        .with_visible(false)
        .build(&events)?;
    let servo = native::engine(proxy.clone(), config_dir)?;
    let chrome = WebView::new(
        &events,
        &window,
        &servo,
        proxy.clone(),
        None,
        ViewSource::Address(CHROME_ORIGIN),
        tracker_exceptions.clone(),
    )?;
    chrome.set_visible(true)?;
    let mut browser = Browser {
        window,
        chrome,
        proxy,
        tabs: Vec::new(),
        active: 0,
        split: None,
        next_id: 1,
        closed: Vec::new(),
        panel: None,
        find: false,
        ready: false,
        state,
        tracker_exceptions,
        picking: None,
        state_path,
        private_window,
        _profile_lock: lock,
        servo,
    };
    for mut saved in std::mem::take(&mut browser.state.tabs) {
        if !allowed(&saved.page.url) {
            saved.page = Page {
                url: BLANK.into(),
                title: String::new(),
            };
        }
        browser.add(saved, false);
    }
    if browser.tabs.is_empty() {
        browser.blank(private_window);
    }
    if let Some([left, right]) = browser.state.split
        && left != right
        && let (Some(left), Some(right)) = (browser.tabs.get(left), browser.tabs.get(right))
    {
        browser.split = Some([left.id, right.id]);
    }
    let active = browser.tabs[browser.state.selected.min(browser.tabs.len() - 1)].id;
    browser.select(&events, active, false)?;
    if let Some(url) = initial
        .as_deref()
        .and_then(|text| text.to_str())
        .and_then(|text| address(text, browser.state.engine))
    {
        let id = if browser.active().view.is_none() {
            active
        } else {
            browser.blank(private_window)
        };
        browser.select(&events, id, false)?;
        browser.navigate(&events, &url)?;
    }
    browser.window.set_visible(true);
    let exit_code = events.run_return(|event, target, control| {
        *control = ControlFlow::Wait;
        let result = match event {
            TaoEvent::UserEvent(event) => browser.event(target, event),
            TaoEvent::WindowEvent {
                window_id, event, ..
            } if window_id == browser.window.id() => match event {
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                    browser.layout()
                }
                WindowEvent::CloseRequested => {
                    if let Err(error) = browser.persist() {
                        error_dialog(&format!(
                            "Could not save this session. The window will close.\n{error}"
                        ));
                    }
                    *control = ControlFlow::Exit;
                    Ok(())
                }
                _ => Ok(()),
            },
            TaoEvent::WindowEvent {
                window_id, event, ..
            } => {
                if let Some(view) = std::iter::once(&mut browser.chrome)
                    .chain(browser.tabs.iter_mut().filter_map(|tab| tab.view.as_mut()))
                    .find(|view| view.id() == window_id)
                {
                    view.window_event(event);
                }
                Ok(())
            }
            TaoEvent::RedrawRequested(window_id) => {
                if let Some(view) = std::iter::once(&browser.chrome)
                    .chain(browser.tabs.iter().filter_map(|tab| tab.view.as_ref()))
                    .find(|view| view.id() == window_id)
                {
                    view.paint();
                }
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(error) = result {
            browser.notify(&error.to_string());
        }
    });
    drop(browser);
    if exit_code != 0 {
        return Err(format!("The Windows event loop exited with code {exit_code}").into());
    }
    Ok(())
}

fn error_dialog(message: &str) {
    let message: Vec<u16> = message.encode_utf16().chain([0]).collect();
    let title: Vec<u16> = "RustSearch".encode_utf16().chain([0]).collect();
    // SAFETY: Both strings are null-terminated and live for the duration of the call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_ICONERROR,
        );
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        error_dialog(&error.to_string());
        std::process::exit(1);
    }
}
