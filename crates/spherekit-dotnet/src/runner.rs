//! The native window loop driven by C# callbacks.

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::ptr;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use spherekit::core::{px, relative, size};
use spherekit::platform::{
    App, AppContext, AppHandler, RedrawPolicy, WindowAttributes, WindowEvent, WindowId,
};
use spherekit::ui::{AnyElement, InputTranslator, IntoElement, ParentElement, Styled, Theme, div};
use spherekit::{SphereKitSurface, SurfaceOptions};
use spherekit_react::{EventQueue, ReactHost};

/// Managed callback that returns an owned UTF-8 JSON tree.
pub(crate) type RenderCallback = unsafe extern "C" fn(*mut c_void) -> *const c_char;
/// Managed callback that receives one native event. Zero means success.
pub(crate) type EventCallback =
    unsafe extern "C" fn(*mut c_void, *const c_char, u64, *const c_char) -> u8;
/// Managed callback that releases a string previously returned by render.
pub(crate) type FreeCallback = unsafe extern "C" fn(*mut c_void, *const c_char);

/// Set by the exported `quit` call. There is one standalone event loop per
/// process, matching the Python binding's one-window contract.
static QUIT: AtomicBool = AtomicBool::new(false);

pub(crate) fn request_quit() {
    QUIT.store(true, Ordering::Relaxed);
}

fn clear_quit() {
    QUIT.store(false, Ordering::Relaxed);
}

#[derive(Clone, Debug)]
pub(crate) struct RunConfig {
    pub(crate) title: String,
    pub(crate) width: f32,
    pub(crate) height: f32,
    pub(crate) css: String,
    pub(crate) dark: bool,
    pub(crate) frame_limit: Option<u64>,
}

#[derive(Default)]
struct Outcome {
    frames: u64,
}

pub(crate) fn run_loop(
    config: RunConfig,
    render: RenderCallback,
    on_event: EventCallback,
    free_string: FreeCallback,
    user_data: *mut c_void,
) -> Result<u64, String> {
    clear_quit();
    if !config.css.is_empty() {
        spherekit_css::Stylesheet::parse(&config.css)
            .map_err(|error| format!("stylesheet: {error}"))?;
    }

    let outcome = Rc::new(RefCell::new(Outcome::default()));
    let app = DotnetApp::new(config, render, on_event, free_string, user_data, Rc::clone(&outcome));
    App::new(app).run().map_err(|error| format!("event loop failed: {error}"))?;
    Ok(outcome.borrow().frames)
}

struct DotnetApp {
    config: RunConfig,
    render: RenderCallback,
    on_event: EventCallback,
    free_string: FreeCallback,
    user_data: *mut c_void,
    window: Option<Arc<spherekit::platform::Window>>,
    surface: Option<SphereKitSurface>,
    host: ReactHost,
    events: EventQueue,
    input: InputTranslator,
    started: Instant,
    last_tree: String,
    revision: u64,
    failure: Option<String>,
    outcome: Rc<RefCell<Outcome>>,
}

impl DotnetApp {
    fn new(
        config: RunConfig,
        render: RenderCallback,
        on_event: EventCallback,
        free_string: FreeCallback,
        user_data: *mut c_void,
        outcome: Rc<RefCell<Outcome>>,
    ) -> Self {
        Self {
            config,
            render,
            on_event,
            free_string,
            user_data,
            window: None,
            surface: None,
            host: ReactHost::new(),
            events: EventQueue::new(),
            input: InputTranslator::new(),
            started: Instant::now(),
            last_tree: String::new(),
            revision: 0,
            failure: None,
            outcome,
        }
    }

    fn theme(&self) -> Theme {
        if self.config.dark { Theme::dark() } else { Theme::light() }
    }

    fn pull_tree(&mut self) {
        if self.failure.is_some() {
            return;
        }
        let pointer = unsafe { (self.render)(self.user_data) };
        if pointer.is_null() {
            self.failure = Some("render callback returned null".to_owned());
            return;
        }
        // Copy before calling the managed free callback. A callback exception
        // never crosses this ABI: C# records it and returns null instead.
        let json = unsafe { CStr::from_ptr(pointer) }.to_string_lossy().into_owned();
        unsafe { (self.free_string)(self.user_data, pointer) };
        if json == self.last_tree {
            return;
        }

        self.revision += 1;
        let stamped = stamp_revision(&json, self.revision);
        match self.host.commit_json(&stamped) {
            Ok(()) => self.last_tree = json,
            Err(error) => eprintln!("spherekit: commit refused: {error}"),
        }
    }

    fn drain_events(&mut self) {
        if self.events.is_empty() || self.failure.is_some() {
            return;
        }
        for event in self.events.drain() {
            let name = match CString::new(event.event) {
                Ok(name) => name,
                Err(_) => {
                    self.failure = Some("native event name contained NUL".to_owned());
                    return;
                }
            };
            let payload = match event.payload {
                Some(payload) => match serde_json::to_string(&payload) {
                    Ok(payload) => Some(CString::new(payload).expect("JSON has no raw NUL")),
                    Err(error) => {
                        self.failure = Some(format!("event payload failed to encode: {error}"));
                        return;
                    }
                },
                None => None,
            };
            let ok = unsafe {
                (self.on_event)(
                    self.user_data,
                    name.as_ptr(),
                    event.node_id,
                    payload.as_ref().map_or(ptr::null(), |value| value.as_ptr()),
                )
            };
            if ok == 0 {
                self.failure = Some("event callback returned failure".to_owned());
                return;
            }
        }
    }

    fn build(&self) -> AnyElement {
        let theme = self.theme();
        div()
            .flex_col()
            .w(relative(1.0))
            .h(relative(1.0))
            .bg(theme.colors.background)
            .child(
                div()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(self.host.ui_element_with_events(&self.events)),
            )
            .into_element()
    }

    fn draw(&mut self) {
        self.pull_tree();
        let root = self.build();
        let clear = self.theme().colors.background;
        let Some(surface) = self.surface.as_mut() else { return };
        match surface.render(root, clear) {
            Ok(Some(_)) => self.outcome.borrow_mut().frames += 1,
            Ok(None) => {}
            Err(error) => eprintln!("spherekit: frame failed: {error}"),
        }
        self.drain_events();
    }

    fn should_exit(&self) -> bool {
        QUIT.load(Ordering::Relaxed)
            || self.failure.is_some()
            || self.config.frame_limit.is_some_and(|limit| self.outcome.borrow().frames >= limit)
    }
}

impl AppHandler for DotnetApp {
    fn resumed(&mut self, cx: &mut AppContext<'_>) {
        if self.surface.is_some() {
            return;
        }
        if !self.config.css.is_empty()
            && let Err(error) = self.host.set_stylesheet(&self.config.css)
        {
            self.failure = Some(format!("stylesheet: {error}"));
            cx.exit();
            return;
        }

        let attrs = WindowAttributes::new(&self.config.title)
            .with_inner_size(size(px(self.config.width), px(self.config.height)))
            .with_min_inner_size(size(px(320.0), px(240.0)))
            .with_visible(false);
        let window = match cx.create_window(&attrs) {
            Ok(window) => window,
            Err(error) => {
                self.failure = Some(format!("failed to create a window: {error}"));
                cx.exit();
                return;
            }
        };
        match pollster::block_on(SphereKitSurface::new(
            Arc::clone(&window),
            window.physical_size(),
            window.scale_factor(),
            SurfaceOptions::default(),
        )) {
            Ok(mut surface) => {
                surface.set_theme(self.theme());
                self.surface = Some(surface);
            }
            Err(error) => {
                self.failure = Some(format!("failed to create GPU surface: {error}"));
                cx.exit();
                return;
            }
        }

        self.window = Some(window);
        cx.scheduler_mut().set_floor(RedrawPolicy::Idle);
        self.draw();
        if self.should_exit() {
            cx.exit();
            return;
        }
        if let Some(window) = self.window.as_ref() {
            window.set_visible(true);
            window.request_redraw();
        }
    }

    fn window_event(&mut self, cx: &mut AppContext<'_>, _id: WindowId, event: WindowEvent) {
        self.input.set_time(self.started.elapsed().as_millis() as u64);
        match &event {
            WindowEvent::CloseRequested => {
                cx.exit();
                return;
            }
            WindowEvent::Resized(new_size) => {
                if let (Some(surface), Some(window)) = (self.surface.as_mut(), self.window.as_ref())
                {
                    let _ = surface.resize(*new_size, window.scale_factor());
                }
                self.draw();
                return;
            }
            WindowEvent::ScaleFactorChanged(scale) => {
                if let (Some(surface), Some(window)) = (self.surface.as_mut(), self.window.as_ref())
                {
                    let _ = surface.resize(window.physical_size(), *scale);
                }
                self.draw();
                return;
            }
            WindowEvent::RedrawRequested => {
                if !self.should_exit() {
                    self.draw();
                }
                return;
            }
            _ => {}
        }

        let mut needs_redraw = false;
        for ui_event in self.input.translate(&event) {
            if let Some(surface) = self.surface.as_mut() {
                let result = surface.dispatch(&ui_event);
                needs_redraw |= result.repaint || result.relayout || result.focus_changed;
            }
        }
        self.drain_events();
        if self.should_exit() {
            cx.exit();
            return;
        }
        if (needs_redraw || !self.events.is_empty())
            && let Some(window) = self.window.as_ref()
        {
            window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, cx: &mut AppContext<'_>) {
        if self.should_exit() {
            cx.exit();
            return;
        }
        if self.config.frame_limit.is_some() {
            self.draw();
            if !self.should_exit()
                && let Some(window) = self.window.as_ref()
            {
                window.request_redraw();
            }
            return;
        }
        let animating = self
            .surface
            .as_ref()
            .is_some_and(|surface| surface.stats().animating || surface.needs_paint());
        if animating && let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn exiting(&mut self, _cx: &mut AppContext<'_>) {
        self.surface = None;
        self.window = None;
    }
}

fn stamp_revision(json: &str, revision: u64) -> String {
    if json.contains("\"revision\"") {
        if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(json)
            && let Some(object) = value.as_object_mut()
        {
            object.insert("revision".to_owned(), serde_json::Value::from(revision));
            return value.to_string();
        }
        return json.to_owned();
    }
    let trimmed = json.trim_start();
    match trimmed.strip_prefix('{') {
        Some(rest) => format!("{{\"revision\":{revision},{rest}"),
        None => json.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_is_owned_by_the_native_side() {
        let stamped = stamp_revision(r#"{"children":[]}"#, 7);
        let value: serde_json::Value = serde_json::from_str(&stamped).expect("parses");
        assert_eq!(value["revision"], 7);
    }

    #[test]
    fn a_supplied_revision_is_replaced() {
        let stamped = stamp_revision(r#"{"revision":99,"children":[]}"#, 3);
        let value: serde_json::Value = serde_json::from_str(&stamped).expect("parses");
        assert_eq!(value["revision"], 3);
    }
}
