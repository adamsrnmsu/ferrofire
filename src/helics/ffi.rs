//! The only place libhelics is touched: the library is `dlopen`ed at run time (no build or link dependency, so the default
//! build needs no HELICS at all) and the ~25 C API functions ferrofire uses are looked up by name. Everything public is
//! safe; every `unsafe` block says what keeps it sound. All branching logic lives in `super` behind the `Federate` trait.
#![allow(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

use super::{Federate, Plan};
use crate::model::{Kind, Value};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::Arc;
use std::time::Duration;

type P = *mut c_void;

/// `HelicsError`: filled in by a failing call, `code` 0 means success.
#[repr(C)]
struct HErr {
    code: c_int,
    msg: *const c_char,
}

impl HErr {
    fn new() -> HErr {
        HErr {
            code: 0,
            msg: std::ptr::null(),
        }
    }

    fn check(&self) -> Result<(), String> {
        if self.code == 0 {
            return Ok(());
        }
        let msg = if self.msg.is_null() {
            String::new()
        } else {
            // SAFETY: on failure libhelics points `msg` at a NUL-terminated string that outlives this call (it is copied here).
            unsafe { CStr::from_ptr(self.msg) }
                .to_string_lossy()
                .into_owned()
        };
        Err(format!("helics: {msg} (code {})", self.code))
    }
}

/// Look `name` up in the library `h`; `T` is the `extern "C" fn` type declared next to the name in `api!`.
fn sym<T: Copy>(h: P, name: &str) -> Result<T, String> {
    assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<P>());
    let c = CString::new(name).map_err(|e| e.to_string())?;
    // SAFETY: `h` is a live dlopen handle (never closed) and `c` is NUL-terminated.
    let p = unsafe { libc::dlsym(h, c.as_ptr()) };
    if p.is_null() {
        return Err(format!("libhelics lacks {name}: not a HELICS 3 library"));
    }
    // SAFETY: `T` is a function pointer type of the same size as `p` (asserted), and the C signature written in `api!`
    // is the one in helics.h for this name.
    Ok(unsafe { std::mem::transmute_copy::<P, T>(&p) })
}

macro_rules! api {
    ($($f:ident: $n:literal $t:ty),* $(,)?) => {
        struct Api { $($f: $t),* }
        impl Api {
            fn load(h: P) -> Result<Api, String> {
                Ok(Api { $($f: sym::<$t>(h, $n)?),* })
            }
        }
    };
}

api! {
    version: "helicsGetVersion" unsafe extern "C" fn() -> *const c_char,
    fi_new: "helicsCreateFederateInfo" unsafe extern "C" fn() -> P,
    fi_free: "helicsFederateInfoFree" unsafe extern "C" fn(P),
    fi_core: "helicsFederateInfoSetCoreTypeFromString" unsafe extern "C" fn(P, *const c_char, *mut HErr),
    fi_init: "helicsFederateInfoSetCoreInitString" unsafe extern "C" fn(P, *const c_char, *mut HErr),
    fi_time: "helicsFederateInfoSetTimeProperty" unsafe extern "C" fn(P, c_int, f64, *mut HErr),
    fi_flag: "helicsFederateInfoSetFlagOption" unsafe extern "C" fn(P, c_int, c_int, *mut HErr),
    fed_new: "helicsCreateValueFederate" unsafe extern "C" fn(*const c_char, P, *mut HErr) -> P,
    fed_free: "helicsFederateFree" unsafe extern "C" fn(P),
    fed_finalize: "helicsFederateFinalize" unsafe extern "C" fn(P, *mut HErr),
    reg_pub: "helicsFederateRegisterGlobalPublication" unsafe extern "C" fn(P, *const c_char, c_int, *const c_char, *mut HErr) -> P,
    reg_sub: "helicsFederateRegisterSubscription" unsafe extern "C" fn(P, *const c_char, *const c_char, *mut HErr) -> P,
    exec_async: "helicsFederateEnterExecutingModeAsync" unsafe extern "C" fn(P, *mut HErr),
    exec_done: "helicsFederateEnterExecutingModeComplete" unsafe extern "C" fn(P, *mut HErr),
    time_async: "helicsFederateRequestTimeAsync" unsafe extern "C" fn(P, f64, *mut HErr),
    time_done: "helicsFederateRequestTimeComplete" unsafe extern "C" fn(P, *mut HErr) -> f64,
    async_ready: "helicsFederateIsAsyncOperationCompleted" unsafe extern "C" fn(P, *mut HErr) -> c_int,
    updated: "helicsInputIsUpdated" unsafe extern "C" fn(P) -> c_int,
    get_double: "helicsInputGetDouble" unsafe extern "C" fn(P, *mut HErr) -> f64,
    get_bool: "helicsInputGetBoolean" unsafe extern "C" fn(P, *mut HErr) -> c_int,
    put_double: "helicsPublicationPublishDouble" unsafe extern "C" fn(P, f64, *mut HErr),
    put_bool: "helicsPublicationPublishBoolean" unsafe extern "C" fn(P, c_int, *mut HErr),
    broker_new: "helicsCreateBroker" unsafe extern "C" fn(*const c_char, *const c_char, *const c_char, *mut HErr) -> P,
    broker_disconnect: "helicsBrokerDisconnect" unsafe extern "C" fn(P, *mut HErr),
    broker_free: "helicsBrokerFree" unsafe extern "C" fn(P),
    close_library: "helicsCloseLibrary" unsafe extern "C" fn(),
}

/// A call that takes a `HelicsError*` last: the error is checked, the result returned in `Ok`.
macro_rules! call {
    ($api:expr, $f:ident($($a:expr),*)) => {{
        let mut e = HErr::new();
        // SAFETY: the function pointer was resolved from libhelics by `sym`, every handle argument is a live handle owned by
        // the caller's struct, strings are NUL-terminated `CString`s, and `e` outlives the call.
        let r = unsafe { ($api.$f)($($a,)* &mut e) };
        e.check().map(|()| r)
    }};
}

fn cstr(s: &str) -> Result<CString, String> {
    CString::new(s).map_err(|_| format!("helics: '{}' has a NUL byte", crate::hint::show(s)))
}

/// A loaded libhelics. The handle is never `dlclose`d (HELICS keeps threads of its own), so the function pointers stay valid.
pub struct Lib {
    api: Api,
}

// SAFETY: `Api` is plain function pointers into a library that is never unloaded; libhelics' C API is thread-safe per handle.
unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

const GET_IT: &str = "get libhelics from https://github.com/GMLC-TDC/HELICS/releases (Helics-shared-*-Linux-*.tar.gz, lib64/libhelics.so) or `pip download helics`, then point at it with `helics: {library: PATH}`, --helics-lib or $FERROFIRE_HELICS_LIB";

fn dlopen(name: &str) -> Result<P, String> {
    let c = cstr(name)?;
    // SAFETY: `c` is NUL-terminated; dlopen has no other preconditions.
    let h = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if !h.is_null() {
        return Ok(h);
    }
    // SAFETY: dlerror returns null or a NUL-terminated string valid until the next dl* call; it is copied here.
    let why = unsafe {
        libc::dlerror()
            .as_ref()
            .map(|e| CStr::from_ptr(e).to_string_lossy().into_owned())
    };
    Err(why.unwrap_or_else(|| format!("cannot load {name}")))
}

impl Lib {
    /// Load libhelics from `path`, else `$FERROFIRE_HELICS_LIB`, else the loader's search for `libhelics.so.3` / `libhelics.so`.
    pub fn open(path: Option<&str>) -> Result<Arc<Lib>, String> {
        let from_env = std::env::var("FERROFIRE_HELICS_LIB").ok();
        let names = match path.map(String::from).or(from_env) {
            Some(p) => vec![p],
            None => vec!["libhelics.so.3".into(), "libhelics.so".into()],
        };
        let mut why = String::new();
        for n in &names {
            match dlopen(n) {
                Ok(h) => return Lib::from_handle(h),
                Err(e) => why = e,
            }
        }
        Err(format!("helics: {why}; {GET_IT}"))
    }

    fn from_handle(h: P) -> Result<Arc<Lib>, String> {
        let lib = Lib { api: Api::load(h)? };
        let v = lib.version();
        if !v.starts_with("3.") {
            return Err(format!("helics: libhelics {v} found, ferrofire needs 3.x"));
        }
        Ok(Arc::new(lib))
    }

    /// `3.7.1 (2025-...)`.
    pub fn version(&self) -> String {
        // SAFETY: helicsGetVersion returns a static NUL-terminated string.
        unsafe { CStr::from_ptr((self.api.version)()) }
            .to_string_lossy()
            .into_owned()
    }

    /// Join the threads libhelics started (cores, brokers, timers), as helics.h asks for before the process ends: libhelics' own
    /// static destructors race those threads at `exit`, and a process that leaves with some running crashes about one run in
    /// forty (ww-20t). The library stays usable for new objects.
    ///
    /// # Safety
    ///
    /// No `Broker` or `Fed` from any `Lib` (every `Lib::open` shares the one libhelics) may be alive or in use on any thread:
    /// libhelics finalizes and frees every object it handed out, so a later `Drop` of one is a use after free. A federate left
    /// mid-call (abandoned on STOP) is finalized here too, and if its peer never comes this call blocks.
    pub unsafe fn close(&self) {
        // SAFETY: helicsCloseLibrary takes no arguments; the caller guarantees no handle outlives it (see # Safety).
        unsafe { (self.api.close_library)() }
    }

    /// Start a broker in this process (the integration tests' federation hub); `init` is HELICS's command line, e.g. `-f 2`.
    pub fn broker(self: &Arc<Lib>, core: &str, name: &str, init: &str) -> Result<Broker, String> {
        let h = call!(
            self.api,
            broker_new(
                cstr(core)?.as_ptr(),
                cstr(name)?.as_ptr(),
                cstr(init)?.as_ptr()
            )
        )?;
        Ok(Broker {
            lib: self.clone(),
            h,
        })
    }
}

pub struct Broker {
    lib: Arc<Lib>,
    h: P,
}

impl Drop for Broker {
    fn drop(&mut self) {
        let _ = call!(self.lib.api, broker_disconnect(self.h));
        // SAFETY: `h` is this struct's own handle and is not used again.
        unsafe { (self.lib.api.broker_free)(self.h) }
    }
}

/// `HELICS_DATA_TYPE_DOUBLE` / `_BOOLEAN`, `HELICS_PROPERTY_TIME_PERIOD`, `HELICS_FLAG_UNINTERRUPTIBLE` (helics.h).
const T_DOUBLE: c_int = 1;
const T_BOOL: c_int = 7;
const PROP_PERIOD: c_int = 140;
const FLAG_UNINTERRUPTIBLE: c_int = 1;

/// A value federate. Subscriptions and publications are indexed in the order they were added.
pub struct Fed {
    lib: Arc<Lib>,
    h: P,
    subs: Vec<(P, Kind)>,
    pubs: Vec<P>,
    stop: fn() -> bool,
    finalized: bool,
    /// A stop request cut short an async call: HELICS cannot finalize or free a federate in that state without blocking.
    abandoned: bool,
}

// SAFETY: a `Fed` is moved to the one thread that drives it; libhelics handles carry no thread affinity.
unsafe impl Send for Fed {}

impl Fed {
    /// A federate granted time in steps of `period` and never earlier (uninterruptible), so a grant is exactly what was asked
    /// for. `init` is HELICS's core init string (`--broker_address=...`). `stop` is polled while a grant is awaited.
    pub fn new(
        lib: &Arc<Lib>,
        name: &str,
        core: &str,
        init: &str,
        period: f64,
        stop: fn() -> bool,
    ) -> Result<Fed, String> {
        let api = &lib.api;
        // SAFETY: plain constructor, no arguments.
        let fi = unsafe { (api.fi_new)() };
        let made = (|| {
            call!(api, fi_core(fi, cstr(core)?.as_ptr()))?;
            call!(api, fi_init(fi, cstr(init)?.as_ptr()))?;
            call!(api, fi_time(fi, PROP_PERIOD, period))?;
            call!(api, fi_flag(fi, FLAG_UNINTERRUPTIBLE, 1))?;
            call!(api, fed_new(cstr(name)?.as_ptr(), fi))
        })();
        // SAFETY: `fi` is the info created above and is not used again.
        unsafe { (api.fi_free)(fi) };
        Ok(Fed {
            lib: lib.clone(),
            h: made?,
            subs: vec![],
            pubs: vec![],
            stop,
            finalized: false,
            abandoned: false,
        })
    }

    /// The federate a `Plan` describes, with its publications and subscriptions registered.
    pub fn from_plan(lib: &Arc<Lib>, plan: &Plan, stop: fn() -> bool) -> Result<Fed, String> {
        let init = plan
            .broker
            .as_ref()
            .map_or(String::new(), |b| format!("--broker_address={b}"));
        let mut f = Fed::new(lib, &plan.name, &plan.core, &init, plan.period, stop)?;
        for p in &plan.pubs {
            f.add_pub(&p.key, p.kind)?;
        }
        for s in &plan.subs {
            f.add_sub(&s.key, s.spec.kind)?;
        }
        Ok(f)
    }

    pub fn add_pub(&mut self, key: &str, kind: Kind) -> Result<(), String> {
        let ty = if kind == Kind::Binary {
            T_BOOL
        } else {
            T_DOUBLE
        };
        let h = call!(
            self.lib.api,
            reg_pub(self.h, cstr(key)?.as_ptr(), ty, c"".as_ptr())
        )?;
        self.pubs.push(h);
        Ok(())
    }

    pub fn add_sub(&mut self, key: &str, kind: Kind) -> Result<(), String> {
        let h = call!(
            self.lib.api,
            reg_sub(self.h, cstr(key)?.as_ptr(), c"".as_ptr())
        )?;
        self.subs.push((h, kind));
        Ok(())
    }

    /// Wait for the async operation in flight: true when it finished, false when a stop was requested first.
    fn wait(&mut self) -> Result<bool, String> {
        while !(self.stop)() {
            if call!(self.lib.api, async_ready(self.h))? != 0 {
                return Ok(true);
            }
            std::thread::sleep(Duration::from_micros(200));
        }
        self.abandoned = true;
        super::ABANDONED.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(false)
    }
}

impl Federate for Fed {
    fn enter_exec(&mut self) -> Result<(), String> {
        call!(self.lib.api, exec_async(self.h))?;
        if self.wait()? {
            call!(self.lib.api, exec_done(self.h))?;
        }
        Ok(())
    }

    fn read(&mut self, i: usize) -> Result<Option<Value>, String> {
        let (h, kind) = self.subs[i];
        // SAFETY: `h` is a subscription of this live federate.
        if unsafe { (self.lib.api.updated)(h) } == 0 {
            return Ok(None);
        }
        Ok(Some(match kind {
            Kind::Binary => Value::Binary(call!(self.lib.api, get_bool(h))? != 0),
            Kind::Analog => Value::Analog(call!(self.lib.api, get_double(h))?),
        }))
    }

    fn publish(&mut self, i: usize, v: Value) -> Result<(), String> {
        let h = self.pubs[i];
        match v {
            Value::Analog(x) => call!(self.lib.api, put_double(h, x)),
            Value::Binary(b) => call!(self.lib.api, put_bool(h, c_int::from(b))),
        }
    }

    fn request_time(&mut self, t: f64) -> Result<Option<f64>, String> {
        call!(self.lib.api, time_async(self.h, t))?;
        if !self.wait()? {
            return Ok(None);
        }
        call!(self.lib.api, time_done(self.h)).map(Some)
    }

    fn finalize(&mut self) {
        if !std::mem::replace(&mut self.finalized, true) && !self.abandoned {
            let _ = call!(self.lib.api, fed_finalize(self.h));
        }
    }
}

impl Drop for Fed {
    fn drop(&mut self) {
        self.finalize();
        if self.abandoned {
            return; // the process is ending; the OS reclaims what HELICS holds
        }
        // SAFETY: `h` is this struct's own handle and is not used again.
        unsafe { (self.lib.api.fed_free)(self.h) }
    }
}
