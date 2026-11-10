//! Loading the sceptre-generated `config.xml` (SPEC 3.1): one `<field-device>` under `<SCEPTRE>`. Points and tags are
//! typed here once; each `<comms>` child is handed to the module its tag names (`proto::REGISTRY`).

use super::logic::Logic;
use super::proto::{self, Runner, Start};
use super::store::{Layout, Store};
use crate::hint::show;
use crate::model::{Kind, Value};
use roxmltree::Node;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Command-line switches every module sees.
#[derive(Clone, Copy, Default)]
pub struct Opts {
    /// Refuse stock quirks and soft failures: an unknown tag in a mapping is an error, not a warning.
    pub strict: bool,
}

impl Opts {
    /// A problem the stock device logs and carries on past: a `WARN` line, or an error under `--strict`.
    pub fn warn(&self, msg: String) -> Result<(), ConfigError> {
        match self.strict {
            true => Err(ConfigError(msg)),
            false => {
                eprintln!("WARN: {msg}");
                Ok(())
            }
        }
    }
}

/// Every tag of the device and its kind. Modules look their mappings up here and may add device-local tags.
#[derive(Default)]
pub struct Tags {
    pub(super) kinds: HashMap<String, Kind>,
    pub(super) internal: Vec<(String, Value)>,
}

impl Tags {
    pub fn kind(&self, tag: &str) -> Option<Kind> {
        self.kinds.get(tag).copied()
    }

    /// Register a device-local tag (a client module's point). Its name must be new.
    pub fn add_internal(&mut self, name: &str, kind: Kind) -> Result<(), ConfigError> {
        self.insert(name, kind)?;
        let zero = match kind {
            Kind::Binary => Value::Binary(false),
            Kind::Analog => Value::Analog(0.0),
        };
        self.internal.push((name.to_string(), zero));
        Ok(())
    }

    fn insert(&mut self, name: &str, kind: Kind) -> Result<(), ConfigError> {
        match self.kinds.insert(name.to_string(), kind) {
            None => Ok(()),
            Some(_) => Err(ConfigError(format!(
                "tag `{}` is defined twice",
                show(name)
            ))),
        }
    }

    /// Check a protocol mapping `tag` of kind `want`: `Ok(false)` (after a warning) for an unknown tag, which the caller skips;
    /// an error for a tag of the wrong kind, such as a coil on an analog tag.
    pub fn mapped(&self, tag: &str, want: Kind, opts: &Opts) -> Result<bool, ConfigError> {
        match self.kind(tag) {
            Some(k) if k == want => Ok(true),
            Some(k) => Err(ConfigError(format!(
                "tag `{}` is {k:?} but its mapping needs {want:?}",
                show(tag)
            ))),
            None => {
                opts.warn(format!("mapping for unknown tag `{}` skipped", show(tag)))?;
                Ok(false)
            }
        }
    }
}

// ---- XML helpers shared by every module ----------------------------------------------------------------------

/// The first child element called `name`.
pub fn child<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.has_tag_name(name))
}

/// Trimmed text of the child `name`.
pub fn text(n: Node, name: &str) -> Option<String> {
    child(n, name).map(|c| c.text().unwrap_or("").trim().to_string())
}

/// Text of the child `name`, which the config must have.
pub fn require(n: Node, name: &str) -> Result<String, ConfigError> {
    text(n, name).ok_or_else(|| ConfigError(format!("<{}> has no <{name}>", n.tag_name().name())))
}

/// Parsed number in the child `name`; `Ok(None)` when the child is absent.
pub fn number<T: std::str::FromStr>(n: Node, name: &str) -> Result<Option<T>, ConfigError> {
    text(n, name)
        .map(|t| {
            t.parse()
                .map_err(|_| ConfigError(format!("<{name}> `{}` is not a valid number", show(&t))))
        })
        .transpose()
}

// ---- the device ----------------------------------------------------------------------------------------------

/// One `<input>` element: a DISH subscription to the provider.
pub struct InputSpec {
    pub endpoint: String,
    /// Point names it subscribes to.
    pub points: Vec<String>,
}

impl From<Section> for InputSpec {
    fn from((endpoint, points): Section) -> InputSpec {
        InputSpec {
            endpoint,
            points: points.into_iter().map(|(_, name, _)| name).collect(),
        }
    }
}

/// One `<output>` element: a REQ client to the provider. Its position in `Config::outputs` is its owner index.
pub struct OutputSpec {
    pub endpoint: String,
}

pub struct Config {
    pub name: String,
    pub cycle_ms: u64,
    pub store: Store,
    pub logic: Logic,
    pub inputs: Vec<InputSpec>,
    pub outputs: Vec<OutputSpec>,
    /// `(comms tag, module)`.
    pub modules: Vec<(String, Runner)>,
}

/// The generator closes `<modbus-client>` with `</modbus-tcp-client>`; stock's parser lets it pass and roxmltree does not.
pub fn normalise(xml: &str) -> String {
    xml.replace("</modbus-tcp-client>", "</modbus-client>")
}

/// Directories the stock loader tries, in order.
pub const SEARCH: &[&str] = &[".", "..", "/etc/sceptre/", "/usr/share", "/usr/local/share"];

/// An absolute path as is, else `name` under the first of `dirs` that has it.
pub fn find_in(name: &str, dirs: &[&str]) -> Option<PathBuf> {
    let direct = PathBuf::from(name);
    if direct.is_absolute() {
        return direct.exists().then_some(direct);
    }
    dirs.iter()
        .map(|d| PathBuf::from(d).join(name))
        .find(|p| p.exists())
}

/// Points of one `<input>` / `<output>`: `(id, name, kind)`.
type PointDef = (Option<String>, String, Kind);

fn points_of(n: Node) -> Result<Vec<PointDef>, ConfigError> {
    n.children()
        .filter(|c| c.has_tag_name("binary") || c.has_tag_name("analog"))
        .map(|c| {
            let kind = match c.tag_name().name() {
                "binary" => Kind::Binary,
                _ => Kind::Analog,
            };
            Ok((text(c, "id"), require(c, "name")?, kind))
        })
        .collect()
}

/// Every `<tag>` element of the device with its endpoint and points.
fn sections(fd: Node, tag: &str) -> Result<Vec<(String, Vec<PointDef>)>, ConfigError> {
    fd.children()
        .filter(|c| c.has_tag_name(tag))
        .map(|c| Ok((require(c, "endpoint")?, points_of(c)?)))
        .collect()
}

fn kind_named(t: &str) -> Result<Kind, ConfigError> {
    match t {
        "binary" => Ok(Kind::Binary),
        "analog" => Ok(Kind::Analog),
        _ => Err(ConfigError(format!(
            "tag type `{}` is neither binary nor analog",
            show(t)
        ))),
    }
}

/// `<internal-tag>`: `<status>` makes a bool, `<value>` a double; the text, if any, is its initial value.
fn internal_tag(n: Node) -> Option<(String, Value)> {
    let name = text(n, "name")?;
    if let Some(s) = text(n, "status") {
        return Some((name, Value::Binary(s == "true" || s == "1")));
    }
    let x = text(n, "value")?;
    Some((name, Value::Analog(x.parse().unwrap_or(0.0))))
}

/// The `<field-device>` element: the root itself, or its child.
fn device_of<'a, 'i>(root: Node<'a, 'i>) -> Result<Node<'a, 'i>, ConfigError> {
    let fd = match root.has_tag_name("field-device") {
        true => Some(root),
        false => child(root, "field-device"),
    };
    fd.ok_or_else(|| ConfigError("no <field-device> in the config".into()))
}

type Section = (String, Vec<PointDef>);

/// Points of every section (outputs first, so an output's kind and owner win for a name both list) and the `id -> (name, kind)` map.
fn point_layout(
    inputs: &[Section],
    outputs: &[Section],
) -> (Layout, HashMap<String, (String, Kind)>) {
    let owned = outputs.iter().enumerate().map(|(i, o)| (Some(i), o));
    let all = owned.chain(inputs.iter().map(|i| (None, i)));
    let (mut layout, mut ids) = (Layout::default(), HashMap::new());
    for (owner, (_, pts)) in all {
        for (id, name, kind) in pts {
            layout.points.push((name.clone(), *kind, owner));
            if let Some(id) = id {
                ids.entry(id.clone()).or_insert((name.clone(), *kind));
            }
        }
    }
    (layout, ids)
}

/// Start every `<comms>` child that has a module.
fn start_comms(
    fd: Node,
    tags: &mut Tags,
    opts: &Opts,
    registry: &[(&str, Start)],
) -> Result<Vec<(String, Runner)>, ConfigError> {
    let mut modules = vec![];
    for node in child(fd, "comms")
        .into_iter()
        .flat_map(|c| c.children().filter(Node::is_element))
    {
        let tag = node.tag_name().name();
        if let Some(run) = proto::start_module(tag, node, tags, opts, registry)? {
            modules.push((tag.to_string(), run));
        }
    }
    Ok(modules)
}

impl Config {
    pub fn parse(
        xml: &str,
        registry: &[(&str, Start)],
        opts: &Opts,
    ) -> Result<Config, ConfigError> {
        let xml = normalise(xml);
        let doc = roxmltree::Document::parse(&xml)
            .map_err(|e| ConfigError(format!("config is not valid XML: {e}")))?;
        Config::from_device(device_of(doc.root_element())?, registry, opts)
    }

    fn from_device(
        fd: Node,
        registry: &[(&str, Start)],
        opts: &Opts,
    ) -> Result<Config, ConfigError> {
        let (inputs, outputs) = (sections(fd, "input")?, sections(fd, "output")?);
        let (mut layout, ids) = point_layout(&inputs, &outputs);
        let mut tags = Tags::default();
        add_tags(fd, &ids, &mut tags, &mut layout, opts)?;
        let modules = start_comms(fd, &mut tags, opts, registry)?;
        layout.internal.extend(tags.internal.iter().cloned());
        let logic = Logic::parse(&text(fd, "logic").unwrap_or_default(), &tags.kinds, opts)?;
        Ok(Config {
            name: require(fd, "name")?,
            cycle_ms: number(fd, "cycle-time")?.filter(|c| *c > 0).unwrap_or(1000),
            store: Store::new(layout),
            logic,
            inputs: inputs
                .into_iter()
                .map(|(endpoint, p)| InputSpec {
                    endpoint,
                    points: p.into_iter().map(|(_, n, _)| n).collect(),
                })
                .collect(),
            outputs: outputs
                .into_iter()
                .map(|(endpoint, _)| OutputSpec { endpoint })
                .collect(),
            modules,
        })
    }
}

/// `<tags>`: aliases onto points (an alias to an unknown id is logged and dropped) and device-local tags.
fn add_tags(
    fd: Node,
    ids: &HashMap<String, (String, Kind)>,
    tags: &mut Tags,
    layout: &mut Layout,
    opts: &Opts,
) -> Result<(), ConfigError> {
    let Some(section) = child(fd, "tags") else {
        return Ok(());
    };
    for n in section.children().filter(Node::is_element) {
        match n.tag_name().name() {
            "external-tag" => external_tag(n, ids, tags, layout, opts)?,
            "internal-tag" => local_tag(n, tags, layout, opts)?,
            other => opts.warn(format!(
                "unknown element <{}> in <tags> ignored",
                show(other)
            ))?,
        }
    }
    Ok(())
}

fn external_tag(
    n: Node,
    ids: &HashMap<String, (String, Kind)>,
    tags: &mut Tags,
    layout: &mut Layout,
    opts: &Opts,
) -> Result<(), ConfigError> {
    let (name, io) = (require(n, "name")?, require(n, "io")?);
    let want = kind_named(&require(n, "type")?)?;
    let Some((point, kind)) = ids.get(&io) else {
        return opts.warn(format!(
            "Cannot map an alias: tag `{}` names unknown io `{}`",
            show(&name),
            show(&io)
        ));
    };
    if *kind != want {
        return Err(ConfigError(format!(
            "tag `{}` is {want:?} but point `{}` is {kind:?}",
            show(&name),
            show(point)
        )));
    }
    tags.insert(&name, want)?;
    layout.external.push((name, point.clone()));
    Ok(())
}

fn local_tag(
    n: Node,
    tags: &mut Tags,
    layout: &mut Layout,
    opts: &Opts,
) -> Result<(), ConfigError> {
    let Some((name, v)) = internal_tag(n) else {
        return opts.warn("internal-tag without <name> and <status>/<value> ignored".into());
    };
    let kind = match v {
        Value::Binary(_) => Kind::Binary,
        Value::Analog(_) => Kind::Analog,
    };
    tags.insert(&name, kind)?;
    layout.internal.push((name, v));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fd::proto::fake;

    const RTU1: &str = include_str!("../../tests/conformance/rtu-1.xml");

    fn parse(xml: &str) -> Result<Config, ConfigError> {
        Config::parse(xml, fake::REGISTRY, &Opts::default())
    }

    fn strict(xml: &str) -> Result<Config, ConfigError> {
        Config::parse(xml, fake::REGISTRY, &Opts { strict: true })
    }

    /// A device around `body`.
    fn device(body: &str) -> String {
        format!("<SCEPTRE><field-device><name>d</name>{body}</field-device></SCEPTRE>")
    }

    fn err(r: Result<Config, ConfigError>) -> String {
        r.err().expect("expected a load error").0
    }

    #[test]
    fn stock_sample_loads() {
        let (head, rest) = RTU1.split_once("<comms>").unwrap();
        let tail = rest.split_once("</comms>").unwrap().1;
        let cfg = parse(&format!(
            "{head}<comms><fake-server><tag>lock_h</tag></fake-server></comms>{tail}"
        ))
        .unwrap();
        assert_eq!((cfg.name.as_str(), cfg.cycle_ms), ("rtu-1", 200));
        assert_eq!(cfg.inputs[0].endpoint, "udp://*;239.0.0.1:40000");
        assert_eq!(cfg.inputs[0].points, ["lock.height", "lowergate.open"]);
        assert_eq!(cfg.outputs[0].endpoint, "tcp://127.0.0.1:5555");
        assert_eq!(cfg.modules.len(), 1);
        assert_eq!(cfg.modules[0].0, "fake-server");
        let s = &cfg.store;
        assert_eq!(s.get_f64("lock_h"), Some(0.0));
        assert!(s.publish("lock.height", 5.0, 1));
        assert_eq!(s.get_f64("lock_h"), Some(5.0));
        assert_eq!(s.get_f64("uvalve"), Some(0.0));
        assert_eq!(s.get("lgate"), Some(Value::Analog(0.0)));
    }

    #[test]
    fn real_stock_modbus_sample_needs_the_feature() {
        let e = err(parse(RTU1));
        assert!(
            e.contains("<modbus-server> needs the `proto-modbus` feature"),
            "{e}"
        );
    }

    #[test]
    fn defaults_and_shapes() {
        let c = parse(&device("")).unwrap();
        assert_eq!(c.cycle_ms, 1000);
        assert!(c.inputs.is_empty() && c.outputs.is_empty() && c.modules.is_empty());
        assert_eq!(
            parse(&device("<cycle-time>0</cycle-time>"))
                .unwrap()
                .cycle_ms,
            1000
        );
        assert_eq!(
            parse(&device("<cycle-time> 50 </cycle-time>"))
                .unwrap()
                .cycle_ms,
            50
        );
        assert!(err(parse(&device("<cycle-time>fast</cycle-time>")))
            .contains("<cycle-time> `fast` is not a valid number"));
        let c = parse("<field-device><name>root</name></field-device>").unwrap();
        assert_eq!(c.name, "root");
    }

    #[test]
    fn structural_errors() {
        assert!(err(parse("<a><b></a>")).contains("not valid XML"));
        assert!(err(parse("<SCEPTRE/>")).contains("no <field-device>"));
        assert!(err(parse("<SCEPTRE><field-device/></SCEPTRE>"))
            .contains("<field-device> has no <name>"));
        assert!(err(parse(&device(
            "<input><analog><name>a</name></analog></input>"
        )))
        .contains("<input> has no <endpoint>"));
        assert!(err(parse(&device(
            "<input><endpoint>e</endpoint><analog><id>I</id></analog></input>"
        )))
        .contains("<analog> has no <name>"));
    }

    const IO: &str = "<input><endpoint>udp://a</endpoint><analog><id>I0</id><name>p.a</name></analog><binary><id>I1</id><name>p.b</name></binary><analog><name>noid</name></analog></input>\
        <output><endpoint>tcp://x</endpoint><analog><id>O0</id><name>p.o</name></analog></output>\
        <output><endpoint>tcp://y</endpoint><binary><id>O1</id><name>p.q</name></binary></output>";

    #[test]
    fn tags_alias_points_with_their_kinds() {
        let tags =
            "<tags><external-tag><name>ta</name><io>I0</io><type>analog</type></external-tag>\
            <external-tag><name>tb</name><io>I1</io><type>binary</type></external-tag>\
            <external-tag><name>to</name><io>O0</io><type>analog</type></external-tag>\
            <external-tag><name>tq</name><io>O1</io><type>binary</type></external-tag>\
            <internal-tag><name>n</name><value>2.5</value></internal-tag>\
            <internal-tag><name>s</name><status>true</status></internal-tag>\
            <internal-tag><name>z</name><status>false</status></internal-tag>\
            <internal-tag><name>v</name><value>junk</value></internal-tag></tags>";
        let c = parse(&device(&format!("{IO}{tags}"))).unwrap();
        assert_eq!(c.outputs.len(), 2);
        let s = &c.store;
        let all: Vec<_> = s.snapshot().into_iter().map(|(t, _)| t).collect();
        assert_eq!(all, ["ta", "tb", "to", "tq", "n", "s", "z", "v"]);
        assert_eq!(s.get_bool("tb"), Some(false));
        assert_eq!(
            (
                s.get_f64("n"),
                s.get_bool("s"),
                s.get_bool("z"),
                s.get_f64("v")
            ),
            (Some(2.5), Some(true), Some(false), Some(0.0))
        );
        s.write("to", Value::Analog(1.0)).unwrap();
        s.write("tq", Value::Binary(true)).unwrap();
        s.write("ta", Value::Analog(1.0)).unwrap();
        assert_eq!(s.pending(0).len(), 2); // ta (unowned) + to
        assert_eq!(s.pending(1).len(), 2); // ta + tq
        assert_eq!(c.inputs[0].points, ["p.a", "p.b", "noid"]);
    }

    #[test]
    fn a_point_in_both_an_input_and_an_output_is_shared_and_owned_by_the_output() {
        let io = "<input><endpoint>udp://a</endpoint><analog><id>I0</id><name>p</name></analog></input>\
            <output><endpoint>tcp://x</endpoint><analog><id>O0</id><name>p</name></analog></output>";
        let tags =
            "<tags><external-tag><name>ti</name><io>I0</io><type>analog</type></external-tag>\
            <external-tag><name>to</name><io>O0</io><type>analog</type></external-tag></tags>";
        let c = parse(&device(&format!("{io}{tags}"))).unwrap();
        c.store.publish("p", 3.0, 1);
        assert_eq!(c.store.get_f64("to"), Some(3.0));
        c.store.write("ti", Value::Analog(1.0)).unwrap();
        assert_eq!(c.store.pending(0).len(), 1);
        assert_eq!(c.store.pending(1).len(), 0);
    }

    #[test]
    fn tag_errors() {
        let ext = |name: &str, io: &str, ty: &str| {
            format!("{IO}<tags><external-tag><name>{name}</name><io>{io}</io><type>{ty}</type></external-tag></tags>")
        };
        assert!(err(parse(&device(&ext("t", "I0", "binary"))))
            .contains("tag `t` is Binary but point `p.a` is Analog"));
        assert!(
            err(parse(&device(&ext("t", "I0", "string")))).contains("neither binary nor analog")
        );
        let dup = format!(
            "{}<internal-tag><name>t</name><value>1</value></internal-tag>",
            ext("t", "I0", "analog")
        )
        .replace("</tags>", "");
        let dup = format!("{dup}</tags>");
        assert!(err(parse(&device(&dup))).contains("tag `t` is defined twice"));
        assert!(err(parse(&device(
            "<tags><external-tag><name>t</name><type>analog</type></external-tag></tags>"
        )))
        .contains("<external-tag> has no <io>"));
    }

    #[test]
    fn unknown_alias_and_odd_tags_are_warnings_but_strict_errors() {
        let t = "<tags><external-tag><name>t</name><io>NOPE</io><type>analog</type></external-tag><internal-tag><name>bad</name></internal-tag><weird/></tags>";
        let c = parse(&device(t)).unwrap();
        assert!(c.store.snapshot().is_empty());
        assert!(err(strict(&device("<tags><external-tag><name>t</name><io>NOPE</io><type>analog</type></external-tag></tags>"))).contains("Cannot map an alias: tag `t` names unknown io `NOPE`"));
        assert!(err(strict(&device(
            "<tags><internal-tag><name>bad</name></internal-tag></tags>"
        )))
        .contains("internal-tag without"));
        assert!(err(strict(&device("<tags><weird/></tags>"))).contains("unknown element <weird>"));
    }

    fn comms(inner: &str) -> String {
        device(&format!("<tags><internal-tag><name>n</name><value>0</value></internal-tag><internal-tag><name>s</name><status>false</status></internal-tag></tags><comms>{inner}</comms>"))
    }

    #[test]
    fn comms_children_go_to_their_module() {
        let c = parse(&comms(
            "<fake-server><tag>n</tag></fake-server><fake-server><tag>n</tag></fake-server>",
        ))
        .unwrap();
        assert_eq!(c.modules.len(), 2);
        assert!(
            err(parse(&comms("<fake-server><tag>s</tag></fake-server>")))
                .contains("tag `s` is Binary but its mapping needs Analog")
        );
        assert!(err(parse(&comms("<fake-server/>"))).contains("<fake-server> has no <tag>"));
        assert_eq!(
            parse(&comms("<fake-server><tag>ghost</tag></fake-server>"))
                .unwrap()
                .modules
                .len(),
            1
        );
        assert!(err(strict(&comms(
            "<fake-server><tag>ghost</tag></fake-server>"
        )))
        .contains("mapping for unknown tag `ghost` skipped"));
        assert_eq!(parse(&comms("<mystery/>")).unwrap().modules.len(), 0);
        assert!(err(parse(&comms("<dnp3-server/>"))).contains("`proto-dnp3`"));
    }

    #[test]
    fn logic_sees_every_tag_and_its_errors_fail_the_load() {
        let c = parse(&comms("").replace("</comms>", "</comms><logic>n = 1</logic>")).unwrap();
        let _ = c;
        assert!(err(parse(
            &comms("").replace("</comms>", "</comms><logic>n = ((</logic>")
        ))
        .contains("logic line 1"));
    }

    #[test]
    fn the_generators_mismatched_client_close_tag_loads() {
        let xml = "<field-device><name>c</name><comms><modbus-client><x/></modbus-tcp-client></comms></field-device>";
        let e = err(parse(xml));
        assert!(
            e.contains("<modbus-client> needs the `proto-modbus` feature"),
            "{e}"
        );
        assert_eq!(
            normalise("a</modbus-tcp-client>b</modbus-tcp-client>"),
            "a</modbus-client>b</modbus-client>"
        );
    }

    #[test]
    fn find_in_prefers_absolute_then_searches_in_order() {
        let base = std::env::temp_dir().join(format!("fd-find-{}", std::process::id()));
        let (a, b) = (base.join("a"), base.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("c.xml"), "").unwrap();
        std::fs::write(b.join("c.xml"), "").unwrap();
        std::fs::write(b.join("only-b.xml"), "").unwrap();
        let dirs = [a.to_str().unwrap(), b.to_str().unwrap()];
        assert_eq!(find_in("c.xml", &dirs), Some(a.join("c.xml")));
        assert_eq!(find_in("only-b.xml", &dirs), Some(b.join("only-b.xml")));
        assert_eq!(find_in("nope.xml", &dirs), None);
        let abs = b.join("c.xml");
        assert_eq!(
            find_in(abs.to_str().unwrap(), &dirs[..1]),
            Some(abs.clone())
        );
        assert_eq!(find_in(base.join("nope").to_str().unwrap(), &dirs), None);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn tags_mapped_and_add_internal() {
        let mut t = Tags::default();
        t.add_internal("n", Kind::Analog).unwrap();
        t.add_internal("s", Kind::Binary).unwrap();
        assert!(t.add_internal("n", Kind::Analog).is_err());
        assert_eq!(t.kind("s"), Some(Kind::Binary));
        let o = Opts::default();
        assert_eq!(t.mapped("n", Kind::Analog, &o), Ok(true));
        assert_eq!(t.mapped("nope", Kind::Analog, &o), Ok(false));
        assert!(t.mapped("s", Kind::Analog, &o).is_err());
        assert_eq!(
            t.internal,
            [
                ("n".to_string(), Value::Analog(0.0)),
                ("s".to_string(), Value::Binary(false))
            ]
        );
    }
}
