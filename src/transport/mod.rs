//! Selectable transport (provider-spec 11): the request/reply and publish halves of a provider, behind one trait.
//! `zmq` is libzmq REP plus the UDP RADIO/DISH publisher (talks to stock bennu); `nng` is NNG REP0 plus PUB0.
//! Both carry the same bytes: `QUERY=`/`READ=`/`WRITE=` answered with NUL-terminated `ACK=`/`ERR=`, and
//! NUL-terminated `tag:value,...` updates in whole-point chunks.

#[cfg(feature = "nng")]
mod nng;
#[cfg(feature = "zmq")]
mod zmq;

/// Answers one request with one reply (a REP socket must always answer).
pub type Handler = Box<dyn Fn(&[u8]) -> Vec<u8> + Send>;

pub trait Publisher: Send {
    /// Send a `tag:value,...` message: chunked, each chunk NUL-terminated.
    fn publish(&self, msg: &str);
    /// What the startup banner says about the publish side.
    fn describe(&self) -> String;
}

pub trait Transport {
    fn name(&self) -> &'static str;
    /// Publish endpoint used when `--publish` is not given.
    fn default_publish(&self) -> &'static str;
    /// Bind `endpoint` and answer each request with `handler` until `bennu::STOP`.
    fn serve(
        &self,
        endpoint: &str,
        handler: Handler,
    ) -> Result<std::thread::JoinHandle<()>, String>;
    fn publisher(&self, endpoint: &str) -> Result<Box<dyn Publisher>, String>;
}

/// Transports this binary was built with (cargo features of the same names).
pub const COMPILED: &[&str] = &[
    #[cfg(feature = "zmq")]
    "zmq",
    #[cfg(feature = "nng")]
    "nng",
];

const DEFAULT: &str = "zmq";

/// Choose the transport: the flag, else the model / config.ini value, else zmq; it must be compiled in.
pub fn pick(
    flag: Option<&str>,
    configured: Option<&str>,
    compiled: &[&str],
) -> Result<&'static str, String> {
    let want = flag.or(configured).unwrap_or(DEFAULT);
    let name = ["zmq", "nng"]
        .into_iter()
        .find(|n| *n == want)
        .ok_or_else(|| format!("unknown transport `{want}` (zmq or nng)"))?;
    match compiled.contains(&name) {
        true => Ok(name),
        false => Err(format!(
            "transport `{name}` is not in this binary: it needs the `{name}` cargo feature (rebuild with --features {name}); built with: {}",
            compiled.join(", ")
        )),
    }
}

pub fn select(flag: Option<&str>, configured: Option<&str>) -> Result<Box<dyn Transport>, String> {
    Ok(match pick(flag, configured, COMPILED)? {
        #[cfg(feature = "nng")]
        "nng" => Box::new(nng::Nng),
        #[cfg(feature = "zmq")]
        _ => Box::new(zmq::Zmq::new()),
        #[cfg(not(feature = "zmq"))]
        _ => unreachable!("pick only returns compiled transports"),
    })
}
