//! Safe Rust access to an embedded V8 runtime.
//!
//! V8 comes from [`v8`] (rusty_v8), whose build script downloads a prebuilt
//! static library for the target from the denoland/rusty_v8 releases and caches
//! it under `$CARGO_HOME/.rusty_v8`. There is no C++ shim and no DLL: V8 is
//! linked straight into the Rust binary.
//!
//! V8 is a C++ API with thread-affine isolates and stack-scoped local handles,
//! so the public Rust surface deliberately exposes an owned engine instead of
//! leaking `v8::Local<T>` handles. Every method opens its own scopes, keeps all
//! handles inside the call, and returns owned UTF-8 data.
//!
//! ## Why the API is string-in, string-out
//!
//! [`Engine::bind`] and [`Engine::call_global`] both speak `&str`, not a
//! `serde` value or a `v8::Value`. That is not a placeholder. Anything richer
//! forces a shared value model on every host, and every host that embeds this
//! crate already has one — `spherekit-bridge` sends JSON Lines. Keeping the
//! boundary at "one owned UTF-8 string each way" keeps this crate small.
//!
//! ## Why microtasks and platform tasks are pumped by hand
//!
//! V8's default microtask policy runs promise continuations whenever script
//! call depth drops to zero, which puts arbitrary JavaScript in the middle of
//! whatever the host was doing — including a layout pass. The engine switches
//! the isolate to an explicit policy, so [`Engine::run_microtasks`] and
//! [`Engine::pump`] are the only points at which queued JavaScript runs, and
//! the host chooses where in its frame those points are.

#![deny(missing_docs)]

use std::error::Error as StdError;
use std::ffi::c_void;
use std::fmt;
use std::sync::OnceLock;

/// Result type used by the JavaScript engine wrapper.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned by the V8 binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// V8 rejected an argument or failed an operation.
    V8 {
        /// Status code; see the `status` constants in the crate source.
        status: i32,
        /// Human-readable detail.
        message: String,
    },
    /// A JavaScript exception, with whatever position and stack V8 gave us.
    ///
    /// The fields are separate rather than pre-formatted because a REPL wants
    /// the stack on its own lines, an error overlay wants the position to jump
    /// to, and a log line wants neither.
    Exception {
        /// The exception rendered as a string, normally `Error: something`.
        message: String,
        /// The `stack` property of the thrown value, or empty if it had none.
        stack: String,
        /// One-based line of the throw site, or zero when V8 did not know.
        line: u32,
        /// One-based column of the throw site, or zero when V8 did not know.
        column: u32,
        /// Script name the throw site came from, as passed to [`Engine::eval_named`].
        resource: String,
    },
    /// The ICU data file passed to [`Engine::with_icu_data_path`] could not be read.
    InvalidPath,
    /// A [`Engine::bind`] name was not a valid identifier, or was already taken.
    ///
    /// Carries the rejected name, since the caller normally built it from a
    /// table of host functions and needs to know which entry is wrong.
    InvalidBinding(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::V8 { status, message } => write!(f, "V8 error {status}: {message}"),
            Self::Exception { message, line, column, resource, .. } => {
                if resource.is_empty() {
                    f.write_str(message)
                } else {
                    write!(f, "{resource}:{line}:{column}: {message}")
                }
            }
            Self::InvalidPath => f.write_str("V8 ICU data file could not be read"),
            Self::InvalidBinding(name) => write!(
                f,
                "`{name}` is not a usable host function name: it must be a plain ASCII \
                 identifier that is not already defined on globalThis"
            ),
        }
    }
}

impl StdError for Error {}

/// Status codes carried by [`Error::V8`].
mod status {
    /// An argument was rejected (bad UTF-8, a non-callable global, ...).
    pub const INVALID_ARGUMENT: i32 = 1;
    /// V8 or one of its per-engine objects could not be created.
    pub const INITIALIZATION_FAILED: i32 = 2;
}

fn v8_error(status: i32, message: impl Into<String>) -> Error {
    Error::V8 { status, message: message.into() }
}

// `pump` runs on the frame thread. Draining without a bound would let a task
// that reposts itself starve rendering forever, so a stuck queue costs one
// frame of latency instead of the whole UI.
const MAX_PUMPED_TASKS: usize = 1024;

/// Process-wide V8 initialisation. V8 can be initialised exactly once per
/// process, and the platform must outlive every isolate, so it is never torn
/// down: `v8::V8::dispose` at static teardown would race whatever isolate a
/// leaked thread still holds.
static V8_INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();

fn initialize_v8(icu_data: &'static [u8]) -> Result<()> {
    V8_INIT
        .get_or_init(|| {
            // ICU must be in place before `V8::initialize`; the first engine
            // created decides which data the whole process uses.
            v8::icu::set_common_data_78(icu_data)
                .map_err(|code| format!("V8 ICU initialization failed (ICU error {code})"))?;
            let platform = v8::new_default_platform(0, false).make_shared();
            v8::V8::initialize_platform(platform);
            v8::V8::initialize();
            Ok(())
        })
        .clone()
        .map_err(|message| v8_error(status::INITIALIZATION_FAILED, message))
}

/// Reads an `icudtl.dat` into a leaked, 16-byte aligned buffer, as ICU requires.
fn load_icu_data(path: &str) -> Result<&'static [u8]> {
    let bytes = std::fs::read(path).map_err(|_| Error::InvalidPath)?;
    let words: &'static mut [u128] =
        Box::leak(vec![0u128; bytes.len().div_ceil(16)].into_boxed_slice());
    let base = words.as_mut_ptr().cast::<u8>();
    // SAFETY: `words` spans at least `bytes.len()` bytes and lives forever.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), base, bytes.len());
        Ok(std::slice::from_raw_parts(base, bytes.len()))
    }
}

/// A Rust closure reachable from JavaScript.
///
/// Boxed so its address stays put once V8 has captured it in a `v8::External`;
/// the inner box erases the closure's type so one callback serves every
/// binding.
struct HostFunction {
    handler: Box<dyn FnMut(&str) -> std::result::Result<String, String>>,
}

fn throw_error(scope: &mut v8::PinScope, message: &str) {
    let text = v8::String::new(scope, message).unwrap_or_else(|| v8::String::empty(scope));
    let exception = v8::Exception::error(scope, text);
    scope.throw_exception(exception);
}

/// The one V8 callback every binding shares.
///
/// A panic must not unwind into V8, so the closure runs inside
/// [`std::panic::catch_unwind`] and a panic is reported to JavaScript as a
/// thrown error instead.
fn host_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let Ok(external) = v8::Local::<v8::External>::try_from(args.data()) else {
        throw_error(scope, "host function is missing its binding record");
        return;
    };
    let record = external.value().cast::<HostFunction>();
    if record.is_null() {
        throw_error(scope, "host function is missing its binding record");
        return;
    }

    let argument = if args.length() > 0 {
        // A throwing `toString` is the caller's bug, not the host's: returning
        // with its exception still pending rethrows the original error.
        match args.get(0).to_string(scope) {
            Some(text) => text.to_rust_string_lossy(scope),
            None => return,
        }
    } else {
        String::new()
    };

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: the record is owned by the `Engine`, which outlives its
        // isolate, and the engine is borrowed mutably for the duration of any
        // script run, so nothing else touches the closure concurrently.
        let function = unsafe { &mut *record };
        (function.handler)(&argument)
    }));

    match outcome {
        Ok(Ok(value)) => match v8::String::new(scope, &value) {
            Some(value) => rv.set(value.into()),
            None => throw_error(scope, "host function result is too large for a V8 string"),
        },
        Ok(Err(message)) => throw_error(scope, &message),
        Err(_) => throw_error(scope, "host function panicked"),
    }
}

/// True when `name` can be reached from JavaScript as a bare identifier.
///
/// Deliberately stricter than the ECMAScript grammar, which allows Unicode
/// `ID_Start`: host function names are chosen by Rust code, and the check exists
/// to catch `""` or `"app.log"` — both of which V8 would happily accept as
/// property keys and neither of which is callable as `app.log(x)` from script —
/// not to re-implement the identifier grammar.
///
/// Reserved words are *not* rejected. `bind("delete", ..)` produces a global
/// that script can only reach as `globalThis["delete"](x)`, which is a strange
/// thing to want but not a broken one.
fn is_binding_name(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else { return false };
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return false;
    }
    characters
        .all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '$')
}

/// Renders a value as UTF-8; `undefined`/`null`/unconvertible become empty.
fn value_text(scope: &mut v8::PinScope, value: Option<v8::Local<v8::Value>>) -> String {
    match value {
        Some(value) if !value.is_null_or_undefined() => {
            value.to_string(scope).map(|text| text.to_rust_string_lossy(scope)).unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// Turns a caught exception into an [`Error::Exception`]. Position and stack
/// come from two different V8 objects — `Message` knows where, the exception's
/// own `stack` property knows how it got there — so both are read here.
fn take_exception(tc: &mut v8::PinnedRef<'_, v8::TryCatch<v8::HandleScope>>) -> Error {
    let exception = tc.exception();
    let stack = tc.stack_trace();
    let detail = tc.message();

    let mut message = value_text(tc, exception);
    if message.is_empty() {
        message = "JavaScript exception".to_owned();
    }
    let stack = value_text(tc, stack);

    let (mut resource, mut line, mut column) = (String::new(), 0, 0);
    if let Some(detail) = detail {
        let name = detail.get_script_resource_name(tc);
        resource = value_text(tc, name);
        line = detail.get_line_number(tc).map_or(0, |line| line as u32);
        // V8 counts columns from zero and lines from one. Both are reported
        // one-based so `resource:line:column` reads like every other
        // compiler's diagnostic.
        column = detail.get_start_column() as u32 + 1;
    }
    Error::Exception { message, stack, line, column, resource }
}

/// Converts the result of a script or call to an owned string.
fn result_text(
    tc: &mut v8::PinnedRef<'_, v8::TryCatch<v8::HandleScope>>,
    value: v8::Local<v8::Value>,
) -> Result<String> {
    match value.to_string(tc) {
        Some(text) => Ok(text.to_rust_string_lossy(tc)),
        None if tc.has_caught() => Err(take_exception(tc)),
        None => Err(v8_error(status::INVALID_ARGUMENT, "V8 result conversion to string failed")),
    }
}

fn new_string<'s>(
    scope: &v8::PinScope<'s, '_>,
    text: &str,
    what: &str,
) -> Result<v8::Local<'s, v8::String>> {
    v8::String::new(scope, text)
        .ok_or_else(|| v8_error(status::INVALID_ARGUMENT, format!("V8 {what} is too large")))
}

fn eval_in(scope: &mut v8::PinScope, source: &str, name: &str) -> Result<String> {
    v8::tc_scope!(let tc, scope);
    let source = new_string(tc, source, "source")?;
    let name = new_string(tc, name, "script name")?;
    let origin =
        v8::ScriptOrigin::new(tc, name.into(), 0, 0, false, 0, None, false, false, false, None);
    let Some(script) = v8::Script::compile(tc, source, Some(&origin)) else {
        return Err(take_exception(tc));
    };
    let Some(value) = script.run(tc) else {
        return Err(take_exception(tc));
    };
    result_text(tc, value)
}

fn bind_in(scope: &mut v8::PinScope, name: &str, record: *mut HostFunction) -> Result<()> {
    v8::tc_scope!(let tc, scope);
    let context = tc.get_current_context();
    let global = context.global(tc);
    let key = new_string(tc, name, "binding name")?;

    match global.has_own_property(tc, key.into()) {
        Some(false) => {}
        Some(true) => return Err(Error::InvalidBinding(name.to_owned())),
        None => return Err(take_exception(tc)),
    }

    let data = v8::External::new(tc, record.cast::<c_void>());
    let Some(function) = v8::Function::builder(host_callback).data(data.into()).build(tc) else {
        return Err(take_exception(tc));
    };
    function.set_name(key);
    match global.set(tc, key.into(), function.into()) {
        Some(true) => Ok(()),
        _ if tc.has_caught() => Err(take_exception(tc)),
        _ => Err(Error::InvalidBinding(name.to_owned())),
    }
}

fn call_global_in(scope: &mut v8::PinScope, name: &str, argument: &str) -> Result<Option<String>> {
    v8::tc_scope!(let tc, scope);
    let context = tc.get_current_context();
    let global = context.global(tc);
    let key = new_string(tc, name, "global name")?;
    let Some(value) = global.get(tc, key.into()) else {
        return Err(take_exception(tc));
    };
    if value.is_null_or_undefined() {
        return Ok(None);
    }
    // Present but uncallable is a mistake worth surfacing: an optional
    // callback is absent, not a number.
    let Ok(function) = v8::Local::<v8::Function>::try_from(value) else {
        return Err(v8_error(status::INVALID_ARGUMENT, "global is not a function"));
    };
    let argument = new_string(tc, argument, "argument")?;
    let Some(returned) = function.call(tc, global.into(), &[argument.into()]) else {
        return Err(take_exception(tc));
    };
    result_text(tc, returned).map(Some)
}

fn has_global_in(scope: &mut v8::PinScope, name: &str) -> Result<bool> {
    v8::tc_scope!(let tc, scope);
    let context = tc.get_current_context();
    let global = context.global(tc);
    let key = new_string(tc, name, "global name")?;
    match global.get(tc, key.into()) {
        Some(value) => Ok(value.is_function()),
        None => Err(take_exception(tc)),
    }
}

fn run_microtasks_in(scope: &mut v8::PinScope) -> Result<()> {
    v8::tc_scope!(let tc, scope);
    tc.perform_microtask_checkpoint();
    if tc.has_caught() { Err(take_exception(tc)) } else { Ok(()) }
}

fn pump_in(scope: &mut v8::PinScope) -> Result<()> {
    v8::tc_scope!(let tc, scope);
    let platform = v8::V8::get_current_platform();
    for _ in 0..MAX_PUMPED_TASKS {
        if !v8::Platform::pump_message_loop(&platform, tc, false) {
            break;
        }
    }
    if tc.has_caught() { Err(take_exception(tc)) } else { Ok(()) }
}

/// Enters the engine's isolate and context, then runs `$body` with `$scope`
/// bound to a `&mut v8::PinScope`.
macro_rules! enter {
    ($engine:expr, |$scope:ident| $body:expr) => {{
        let engine = $engine;
        v8::scope!(let handle_scope, &mut engine.isolate);
        let context = v8::Local::new(handle_scope, &engine.context);
        let $scope = &mut v8::ContextScope::new(handle_scope, context);
        $body
    }};
}

/// An owned V8 isolate and execution context.
///
/// An engine is intentionally used through `&mut self`: V8 isolates are
/// entered by one thread at a time, and this prevents accidental concurrent
/// calls from safe Rust without pretending the isolate is `Sync`.
pub struct Engine {
    // Field order is drop order. The context handle goes first, then the
    // isolate; once the isolate is disposed no script can be running, which is
    // what makes dropping the handler boxes afterwards safe.
    context: v8::Global<v8::Context>,
    isolate: v8::OwnedIsolate,
    /// Boxed handlers whose addresses V8 holds as `External`s.
    #[allow(clippy::vec_box, reason = "V8 holds each address; a Vec realloc must not move them")]
    bindings: Vec<Box<HostFunction>>,
}

impl Engine {
    /// Creates an engine using the ICU data embedded in this crate.
    pub fn new() -> Result<Self> {
        Self::create(deno_core_icudata::ICU_DATA)
    }

    /// Creates an engine with an explicit ICU 78 `icudtl.dat`.
    ///
    /// `None` uses the embedded data. ICU is process-wide: only the first
    /// engine created in a process decides which data is loaded.
    pub fn with_icu_data_path(path: Option<&str>) -> Result<Self> {
        match path {
            Some(path) if V8_INIT.get().is_none() => Self::create(load_icu_data(path)?),
            _ => Self::new(),
        }
    }

    fn create(icu_data: &'static [u8]) -> Result<Self> {
        initialize_v8(icu_data)?;
        let mut isolate = v8::Isolate::new(v8::CreateParams::default());
        // The host drives the frame loop, so it also decides when promise
        // continuations run; see the crate docs.
        isolate.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
        let context = {
            v8::scope!(let scope, &mut isolate);
            let context = v8::Context::new(scope, Default::default());
            v8::Global::new(scope, context)
        };
        Ok(Self { context, isolate, bindings: Vec::new() })
    }

    /// Evaluates a UTF-8 JavaScript source string and returns its UTF-8 value.
    ///
    /// The script is named `<eval>`. V8's own default for an unnamed script is
    /// the literal string `undefined`, which is worse than useless in a stack
    /// trace, so even the anonymous case gets an honest name.
    pub fn eval(&mut self, source: &str) -> Result<String> {
        self.eval_named(source, "<eval>")
    }

    /// Evaluates with a script name, so exceptions and stack traces name a real file.
    pub fn eval_named(&mut self, source: &str, name: &str) -> Result<String> {
        enter!(self, |scope| eval_in(scope, source, name))
    }

    /// Installs a Rust closure as a global function taking one string and
    /// returning one string.
    ///
    /// The JavaScript side sees `globalThis[name](argument) -> string`. A
    /// returned `Err` becomes a thrown JavaScript `Error` carrying the message,
    /// which is what makes a host failure catchable by script instead of a
    /// silent `undefined`.
    ///
    /// The closure outlives the call that installed it, so it is kept alive by
    /// the engine and dropped with it. Binding a name that already exists on
    /// `globalThis` fails rather than shadowing it.
    pub fn bind(
        &mut self,
        name: &str,
        handler: impl FnMut(&str) -> std::result::Result<String, String> + 'static,
    ) -> Result<()> {
        if !is_binding_name(name) {
            return Err(Error::InvalidBinding(name.to_owned()));
        }
        let mut record = Box::new(HostFunction { handler: Box::new(handler) });
        let pointer: *mut HostFunction = &mut *record;
        enter!(&mut *self, |scope| bind_in(scope, name, pointer))?;
        // Only now is the record reachable from JavaScript; on any earlier
        // failure it is simply dropped here.
        self.bindings.push(record);
        Ok(())
    }

    /// Calls a global function by name with one string argument, returning its
    /// string result.
    ///
    /// Returns `Ok(None)` when no such global exists, so an optional JavaScript
    /// callback is not an error. A global that exists but is not callable *is*
    /// an error: that is a mistake in the script, not an absent hook.
    pub fn call_global(&mut self, name: &str, argument: &str) -> Result<Option<String>> {
        enter!(self, |scope| call_global_in(scope, name, argument))
    }

    /// True when `globalThis[name]` is callable.
    pub fn has_global(&mut self, name: &str) -> Result<bool> {
        enter!(self, |scope| has_global_in(scope, name))
    }

    /// Drains the microtask queue — resolved promise callbacks run here.
    pub fn run_microtasks(&mut self) -> Result<()> {
        enter!(self, |scope| run_microtasks_in(scope))
    }

    /// Pumps V8's foreground platform tasks. Call once per frame alongside
    /// [`Engine::run_microtasks`].
    ///
    /// Bounded rather than drained to exhaustion: this runs on the thread that
    /// draws, and a task that reposts itself would otherwise hold the frame
    /// hostage.
    pub fn pump(&mut self) -> Result<()> {
        enter!(self, |scope| pump_in(scope))
    }

    /// Returns the version of the linked V8.
    pub fn version() -> String {
        v8::V8::get_version().to_owned()
    }
}

/// Returns the V8 version without creating an isolate.
pub fn v8_version() -> String {
    Engine::version()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// The script the crate ships as its smoke test, evaluated for real below.
    const WELCOME: &str = include_str!("../test/welcome.js");

    fn engine() -> Engine {
        Engine::new().expect("create V8 engine")
    }

    fn exception(error: &Error) -> (&str, &str, u32, u32, &str) {
        match error {
            Error::Exception { message, stack, line, column, resource } => {
                (message.as_str(), stack.as_str(), *line, *column, resource.as_str())
            }
            other => panic!("expected a JavaScript exception, got {other:?}"),
        }
    }

    #[test]
    fn evaluates_javascript() {
        assert_eq!(engine().eval("1 + 2").expect("evaluate script"), "3");
    }

    #[test]
    fn reports_javascript_errors() {
        let error = engine().eval("throw new Error('boom')").expect_err("script should fail");
        assert!(error.to_string().contains("boom"), "{error}");
    }

    #[test]
    fn eval_named_puts_the_script_name_in_the_exception() {
        let error = engine()
            .eval_named("const a = 1;\n\nthrow new Error('boom');", "welcome.js")
            .expect_err("script should fail");
        let (message, _, line, _, resource) = exception(&error);
        assert_eq!(resource, "welcome.js");
        assert_eq!(line, 3);
        assert!(message.contains("boom"), "{message}");
    }

    #[test]
    fn a_thrown_error_carries_a_stack_and_a_plausible_position() {
        let error = engine()
            .eval_named("function inner() { throw new Error('deep'); }\ninner();", "deep.js")
            .expect_err("script should fail");
        let (_, stack, line, column, _) = exception(&error);
        assert!(stack.contains("deep"), "{stack}");
        assert!(stack.contains("deep.js"), "{stack}");
        assert_eq!(line, 1);
        assert!(column > 0, "column should be one-based, got {column}");
    }

    #[test]
    fn a_binding_round_trips_a_string() {
        let mut engine = engine();
        engine.bind("shout", |value| Ok(value.to_uppercase())).expect("bind shout");
        assert_eq!(engine.eval("shout('hello')").expect("call shout"), "HELLO");
    }

    #[test]
    fn a_binding_that_fails_throws_catchable_in_javascript() {
        let mut engine = engine();
        engine.bind("refuse", |_| Err("no thanks".to_owned())).expect("bind refuse");
        let caught = engine
            .eval("try { refuse('x'); 'not thrown' } catch (error) { error.message }")
            .expect("catch the host error");
        assert_eq!(caught, "no thanks");
    }

    #[test]
    fn a_binding_keeps_its_captured_state_across_calls() {
        let mut engine = engine();
        let mut count = 0u32;
        engine
            .bind("tick", move |_| {
                count += 1;
                Ok(count.to_string())
            })
            .expect("bind tick");
        assert_eq!(engine.eval("tick('')").expect("first"), "1");
        assert_eq!(engine.eval("tick('')").expect("second"), "2");
        assert_eq!(engine.eval("[tick(''), tick('')].join(',')").expect("third"), "3,4");
    }

    #[test]
    fn a_binding_sees_what_javascript_passed() {
        let seen = Rc::new(RefCell::new(String::new()));
        let recorder = Rc::clone(&seen);
        let mut engine = engine();
        engine
            .bind("record", move |value| {
                recorder.borrow_mut().push_str(value);
                Ok(String::new())
            })
            .expect("bind record");
        engine.eval("record('payload')").expect("call record");
        assert_eq!(seen.borrow().as_str(), "payload");
    }

    #[test]
    fn binding_an_unusable_name_is_rejected() {
        let mut engine = engine();
        assert_eq!(
            engine.bind("app.log", |_| Ok(String::new())).expect_err("dotted name"),
            Error::InvalidBinding("app.log".to_owned())
        );
        assert_eq!(
            engine.bind("", |_| Ok(String::new())).expect_err("empty name"),
            Error::InvalidBinding(String::new())
        );
        assert_eq!(
            engine.bind("Math", |_| Ok(String::new())).expect_err("taken name"),
            Error::InvalidBinding("Math".to_owned())
        );
    }

    #[test]
    fn binding_the_same_name_twice_is_rejected() {
        let mut engine = engine();
        engine.bind("once", |_| Ok(String::new())).expect("first bind");
        assert_eq!(
            engine.bind("once", |_| Ok(String::new())).expect_err("second bind"),
            Error::InvalidBinding("once".to_owned())
        );
    }

    #[test]
    fn call_global_on_a_missing_global_is_not_an_error() {
        let mut engine = engine();
        assert_eq!(engine.call_global("onFrame", "{}").expect("missing global"), None);
        assert!(!engine.has_global("onFrame").expect("missing global"));
    }

    #[test]
    fn call_global_reaches_a_function_defined_by_a_previous_eval() {
        let mut engine = engine();
        engine.eval("globalThis.greet = (name) => `hi ${name}`;").expect("define greet");
        assert!(engine.has_global("greet").expect("greet exists"));
        assert_eq!(
            engine.call_global("greet", "sphere").expect("call greet"),
            Some("hi sphere".to_owned())
        );
    }

    #[test]
    fn call_global_propagates_an_exception_from_the_callee() {
        let mut engine = engine();
        engine
            .eval_named("globalThis.explode = () => { throw new Error('inside'); };", "host.js")
            .expect("define explode");
        let error = engine.call_global("explode", "").expect_err("callee throws");
        let (message, _, _, _, resource) = exception(&error);
        assert!(message.contains("inside"), "{message}");
        assert_eq!(resource, "host.js");
    }

    #[test]
    fn a_non_callable_global_is_an_error_rather_than_absent() {
        let mut engine = engine();
        engine.eval("globalThis.notAFunction = 42;").expect("define value");
        assert!(!engine.has_global("notAFunction").expect("not callable"));
        assert!(engine.call_global("notAFunction", "").is_err());
    }

    #[test]
    fn run_microtasks_runs_a_queued_promise_continuation() {
        let mut engine = engine();
        engine
            .eval(
                "globalThis.resolved = false;\
                 Promise.resolve().then(() => { globalThis.resolved = true; });",
            )
            .expect("queue a continuation");
        assert_eq!(engine.eval("resolved").expect("still pending"), "false");
        engine.run_microtasks().expect("drain microtasks");
        assert_eq!(engine.eval("resolved").expect("now resolved"), "true");
    }

    #[test]
    fn a_binding_can_be_called_from_a_promise_continuation() {
        let mut engine = engine();
        engine.bind("stamp", |value| Ok(format!("[{value}]"))).expect("bind stamp");
        engine
            .eval("globalThis.out = ''; Promise.resolve('x').then((v) => { out = stamp(v); });")
            .expect("queue a continuation");
        engine.run_microtasks().expect("drain microtasks");
        assert_eq!(engine.eval("out").expect("read out"), "[x]");
    }

    #[test]
    fn pump_is_safe_to_call_with_an_empty_task_queue() {
        let mut engine = engine();
        engine.pump().expect("first pump");
        engine.pump().expect("second pump");
    }

    #[test]
    fn the_welcome_script_runs_and_exposes_its_entry_points() {
        let mut engine = engine();
        let greeting = engine.eval_named(WELCOME, "welcome.js").expect("run welcome.js");
        assert!(greeting.contains("SphereKit"), "{greeting}");
        assert!(engine.has_global("welcome").expect("welcome is callable"));
        assert_eq!(
            engine.call_global("welcome", "world").expect("call welcome"),
            Some("Hello, world! Running on V8 via SphereKit.".to_owned())
        );
        assert_eq!(engine.eval("microtaskRan").expect("not yet"), "false");
        engine.run_microtasks().expect("drain microtasks");
        assert_eq!(engine.eval("microtaskRan").expect("now"), "true");
    }

    #[test]
    fn the_version_is_reported() {
        assert!(Engine::version().starts_with(char::is_numeric), "{}", Engine::version());
        assert_eq!(v8_version(), Engine::version());
    }
}
