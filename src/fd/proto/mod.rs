//! Protocol modules (docs/field-device-protocols-plan.md section 2). A module is selected by the tag name of a `<comms>`
//! child, parses its own element, and serves on the shared `Store` until told to stop. To add one: write
//! `proto/<name>.rs` implementing `Module`, add a `REGISTRY` line behind its `proto-<name>` feature, and list its tag in `KNOWN`.

#[cfg(feature = "proto-dnp3")]
pub mod dnp3;
#[cfg(feature = "proto-iec61850")]
pub mod iec61850;

use super::config::{ConfigError, Opts, Tags};
use super::store::Store;
use roxmltree::Node;
use std::future::Future;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;

pub trait Module: Send + 'static {
    /// Parse this module's own `<comms>` child; an error names the key. Map tags with `Tags::mapped`.
    fn parse(node: Node, tags: &mut Tags, opts: &Opts) -> Result<Self, ConfigError>
    where
        Self: Sized;
    /// Serve until `stop`. A bind failure logs and returns; the rest of the device keeps running (stock). Never panics on input.
    fn run(self, store: Store, stop: CancellationToken) -> impl Future<Output = ()> + Send;
}

pub type Task = Pin<Box<dyn Future<Output = ()> + Send>>;
/// A parsed module, ready to be spawned on the runtime.
pub type Runner = Box<dyn FnOnce(Store, CancellationToken) -> Task + Send>;
pub type Start = fn(Node<'_, '_>, &mut Tags, &Opts) -> Result<Runner, ConfigError>;

/// Parse a node with module `M` into a `Runner`; what a `REGISTRY` line holds.
pub fn start<M: Module>(node: Node, tags: &mut Tags, opts: &Opts) -> Result<Runner, ConfigError> {
    let m = M::parse(node, tags, opts)?;
    Ok(Box::new(move |store, stop| Box::pin(m.run(store, stop))))
}

/// The protocols compiled into this binary: `<comms>` tag -> starter.
pub static REGISTRY: &[(&str, Start)] = &[
    #[cfg(feature = "proto-dnp3")]
    ("dnp3-server", start::<dnp3::Outstation>),
    #[cfg(feature = "proto-iec61850")]
    ("iec61850-server", start::<iec61850::Server>),
];

/// Every `<comms>` tag a build can have, and the Cargo feature that provides it.
pub const KNOWN: &[(&str, &str)] = &[
    ("modbus-server", "proto-modbus"),
    ("modbus-client", "proto-modbus"),
    ("dnp3-server", "proto-dnp3"),
    ("dnp3-client", "proto-dnp3"),
    ("iec60870-5-104-server", "proto-iec104"),
    ("iec60870-5-104-client", "proto-iec104"),
    ("bacnet-server", "proto-bacnet"),
    ("bacnet-client", "proto-bacnet"),
    ("iec61850-server", "proto-iec61850"),
    ("iccp-server", "proto-iccp"),
];

/// Start the module a `<comms>` child names. `Ok(None)`: a tag nothing knows is warned about and skipped; a tag whose feature is
/// not built in is an error naming the feature.
pub fn start_module(
    tag: &str,
    node: Node,
    tags: &mut Tags,
    opts: &Opts,
    registry: &[(&str, Start)],
) -> Result<Option<Runner>, ConfigError> {
    if let Some((_, start)) = registry.iter().find(|(t, _)| *t == tag) {
        return start(node, tags, opts).map(Some);
    }
    match KNOWN.iter().find(|(t, _)| *t == tag) {
        Some((_, feature)) => Err(ConfigError(format!(
            "<{tag}> needs the `{feature}` feature, which this binary was built without"
        ))),
        None => {
            opts.warn(format!(
                "unknown <comms> element <{}> skipped",
                crate::hint::show(tag)
            ))?;
            Ok(None)
        }
    }
}

/// The `--list-protocols` text: compiled modules, then the ones this build lacks.
pub fn list(registry: &[(&str, Start)]) -> String {
    let mut out = String::from("compiled:\n");
    for (tag, _) in registry {
        out.push_str(&format!("  {tag}\n"));
    }
    out.push_str("not built:\n");
    for (tag, feature) in KNOWN
        .iter()
        .filter(|(t, _)| !registry.iter().any(|(r, _)| r == t))
    {
        out.push_str(&format!("  {tag} (feature {feature})\n"));
    }
    out
}

#[cfg(test)]
pub mod fake {
    //! A stand-in module for tests: `<fake-server><tag>t</tag></fake-server>` maps an analog tag and, once running, writes 1.0 to it.
    use super::*;
    use crate::model::{Kind, Value};

    pub struct Fake(pub Option<String>);

    impl Module for Fake {
        fn parse(node: Node, tags: &mut Tags, opts: &Opts) -> Result<Self, ConfigError> {
            let tag = super::super::config::require(node, "tag")?;
            Ok(Fake(tags.mapped(&tag, Kind::Analog, opts)?.then_some(tag)))
        }
        async fn run(self, store: Store, stop: CancellationToken) {
            if let Some(t) = &self.0 {
                let _ = store.write(t, Value::Analog(1.0));
            }
            stop.cancelled().await;
        }
    }

    pub static REGISTRY: &[(&str, Start)] = &[("fake-server", start::<Fake>)];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_shows_compiled_then_missing() {
        assert!(list(&[])
            .starts_with("compiled:\nnot built:\n  modbus-server (feature proto-modbus)\n"));
        let out = list(fake::REGISTRY);
        assert!(
            out.starts_with("compiled:\n  fake-server\nnot built:\n"),
            "{out}"
        );
        let all: Vec<_> = KNOWN.iter().map(|(t, _)| *t).collect();
        let shown = list(&[("dnp3-server", start::<fake::Fake>)]);
        assert!(shown.contains("compiled:\n  dnp3-server\n"));
        assert!(!shown.contains("dnp3-server (feature"));
        assert!(shown.contains("iccp-server (feature proto-iccp)"));
        assert!(all.contains(&"iec61850-server"));
    }

    #[test]
    fn registry_ships_only_what_is_known() {
        assert!(REGISTRY
            .iter()
            .all(|(t, _)| KNOWN.iter().any(|(k, _)| k == t)));
    }

    fn run(tag: &str, registry: &[(&str, Start)], opts: &Opts) -> Result<Option<()>, ConfigError> {
        let xml = format!("<{tag}><tag>x</tag></{tag}>");
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let mut tags = Tags::default();
        tags.add_internal("x", crate::model::Kind::Analog).unwrap();
        start_module(tag, doc.root_element(), &mut tags, opts, registry).map(|r| r.map(|_| ()))
    }

    #[test]
    fn a_registered_tag_starts_a_known_missing_one_names_its_feature_an_unknown_one_is_skipped() {
        let o = Opts::default();
        assert_eq!(run("fake-server", fake::REGISTRY, &o), Ok(Some(())));
        let e = run("dnp3-server", fake::REGISTRY, &o).unwrap_err();
        assert!(
            e.0.contains("<dnp3-server> needs the `proto-dnp3` feature"),
            "{e}"
        );
        assert_eq!(run("whatever-server", fake::REGISTRY, &o), Ok(None));
        let e = run("whatever-server", fake::REGISTRY, &Opts { strict: true }).unwrap_err();
        assert!(
            e.0.contains("unknown <comms> element <whatever-server>"),
            "{e}"
        );
    }
}
