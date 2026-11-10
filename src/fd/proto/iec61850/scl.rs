//! The data model of the IEC 61850 server, generated from the field device's point map as SCL text (the `iec61850-rs`
//! server takes its whole namespace from SCL). Pure functions, no I/O: `Model::scl` is what `Ied::from_scl` is given.
//!
//! One logical device (`<ld-inst>`), one logical node per `<ln>` (default `GGIO1`), one data object per point:
//! binary input -> `Ind` (SPS), analog input -> `AnIn` (MV), binary output -> `SPCSO` (SPC, controllable),
//! analog output -> `APCSO` (APC, controllable). A data set `Events` holds every point's value and quality; an
//! unbuffered report control block and, when configured, a GOOSE control block publish it.

use crate::model::Kind;
use std::fmt::Write;

/// Names are put into XML unescaped, so they are held to this alphabet.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Ind,
    AnIn,
    Spcso,
    Apcso,
}

/// Everything that differs between the four roles, in one place.
struct Info {
    /// The config element.
    element: &'static str,
    /// Data object name stem when the config gives none (`Ind1`, `AnIn2`, ...).
    stem: &'static str,
    do_type: &'static str,
    /// Functional constraint of the value and of its quality and time.
    fc: &'static str,
    /// Value attribute path, MMS (`mag$f`) and data set (`mag.f`) forms.
    path: &'static str,
    da: &'static str,
    kind: Kind,
}

impl Role {
    fn info(self) -> Info {
        let (element, stem, do_type, fc, path, da, kind) = match self {
            Role::Ind => (
                "binary-input",
                "Ind",
                "SPS_T",
                "ST",
                "stVal",
                "stVal",
                Kind::Binary,
            ),
            Role::AnIn => (
                "analog-input",
                "AnIn",
                "MV_T",
                "MX",
                "mag$f",
                "mag.f",
                Kind::Analog,
            ),
            Role::Spcso => (
                "binary-output",
                "SPCSO",
                "SPC_T",
                "ST",
                "stVal",
                "stVal",
                Kind::Binary,
            ),
            Role::Apcso => (
                "analog-output",
                "APCSO",
                "APC_T",
                "MX",
                "mxVal$f",
                "mxVal.f",
                Kind::Analog,
            ),
        };
        Info {
            element,
            stem,
            do_type,
            fc,
            path,
            da,
            kind,
        }
    }
    /// The role of a config element.
    pub fn from_element(name: &str) -> Option<Role> {
        [Role::Ind, Role::AnIn, Role::Spcso, Role::Apcso]
            .into_iter()
            .find(|r| r.info().element == name)
    }
    pub fn kind(self) -> Kind {
        self.info().kind
    }
    pub fn controllable(self) -> bool {
        matches!(self, Role::Spcso | Role::Apcso)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Point {
    pub tag: String,
    pub role: Role,
    /// Logical node class (`GGIO`) and instance number.
    pub class: String,
    pub inst: u16,
    /// Data object name (`Ind1`).
    pub name: String,
    /// Select-before-operate (`ctlModel` 2) instead of direct-with-normal-security (1).
    pub sbo: bool,
}

impl Point {
    pub fn ln(&self) -> String {
        format!("{}{}", self.class, self.inst)
    }
    /// MMS reference of the value attribute: `LD/GGIO1$ST$Ind1$stVal`.
    pub fn value_ref(&self, domain: &str) -> String {
        let i = self.role.info();
        format!("{domain}/{}${}${}${}", self.ln(), i.fc, self.name, i.path)
    }
    /// MMS reference of the control object a client operates: `LD/GGIO1$CO$SPCSO1`.
    pub fn control_ref(&self, domain: &str) -> String {
        format!("{domain}/{}$CO${}", self.ln(), self.name)
    }
    /// MMS reference of the timestamp next to the value.
    pub fn time_ref(&self, domain: &str) -> String {
        format!(
            "{domain}/{}${}${}$t",
            self.ln(),
            self.role.info().fc,
            self.name
        )
    }
}

/// GOOSE control block settings (the SCL `GSE` address and times).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Goose {
    pub interface: String,
    /// `01-0C-CD-01-00-00` style destination.
    pub mac: String,
    pub appid: u16,
    pub vlan_id: Option<u16>,
    pub vlan_priority: u8,
    pub min_ms: u32,
    pub max_ms: u32,
    /// Control block name (`gcbAnalogValues`, the one libiec61850's GOOSE examples subscribe to).
    pub cb_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Model {
    pub ied: String,
    pub ld: String,
    pub points: Vec<Point>,
    pub goose: Option<Goose>,
}

impl Model {
    /// The MMS domain of the logical device: IED name followed by LD inst.
    pub fn domain(&self) -> String {
        format!("{}{}", self.ied, self.ld)
    }

    /// Logical nodes in use, in first-seen order.
    fn nodes(&self) -> Vec<(String, u16)> {
        let mut out: Vec<(String, u16)> = vec![];
        for p in &self.points {
            if !out.iter().any(|(c, i)| *c == p.class && *i == p.inst) {
                out.push((p.class.clone(), p.inst));
            }
        }
        out
    }

    fn fcda(&self, p: &Point) -> String {
        let i = p.role.info();
        let row = |da: &str, fc: &str| {
            format!(
                "<FCDA ldInst=\"{}\" lnClass=\"{}\" lnInst=\"{}\" doName=\"{}\" daName=\"{da}\" fc=\"{fc}\"/>",
                self.ld, p.class, p.inst, p.name
            )
        };
        format!("{}{}", row(i.da, i.fc), row("q", i.fc))
    }

    fn communication(&self, g: &Goose) -> String {
        let mut a = format!(
            "<P type=\"MAC-Address\">{}</P><P type=\"APPID\">{:04X}</P>",
            g.mac, g.appid
        );
        if let Some(v) = g.vlan_id {
            let _ = write!(
                a,
                "<P type=\"VLAN-ID\">{v:03X}</P><P type=\"VLAN-PRIORITY\">{}</P>",
                g.vlan_priority
            );
        }
        format!(
            "<Communication><SubNetwork name=\"process\"><ConnectedAP iedName=\"{}\" apName=\"P1\"><GSE ldInst=\"{}\" cbName=\"{}\"><Address>{a}</Address><MinTime unit=\"s\" multiplier=\"m\">{}</MinTime><MaxTime unit=\"s\" multiplier=\"m\">{}</MaxTime></GSE></ConnectedAP></SubNetwork></Communication>",
            self.ied, self.ld, g.cb_name, g.min_ms, g.max_ms
        )
    }

    #[rustfmt::skip] // keeps the codespell markers on their lines
    fn ln0(&self) -> String {
        let fcdas: String = self.points.iter().map(|p| self.fcda(p)).collect();
        let mut s = format!("<LN0 lnClass=\"LLN0\" inst=\"\" lnType=\"LLN0_T\"><DataSet name=\"Events\">{fcdas}</DataSet>");
        s.push_str("<ReportControl name=\"EventsRCB01\" datSet=\"Events\" confRev=\"1\" indexed=\"false\"><TrgOps dchg=\"true\" qchg=\"true\" period=\"true\"/><OptFields seqNum=\"true\" timeStamp=\"true\" dataSet=\"true\" reasonCode=\"true\" entryID=\"false\" configRef=\"true\"/><RptEnabled max=\"1\"/></ReportControl>"); // codespell:ignore datset
        if let Some(g) = &self.goose {
            let _ = write!(s, "<GSEControl name=\"{}\" datSet=\"Events\" confRev=\"1\" appID=\"{}\" type=\"GOOSE\"/>", g.cb_name, self.ied); // codespell:ignore datset
        }
        s.push_str("</LN0>");
        s
    }

    fn ln(&self, class: &str, inst: u16) -> String {
        let dois: String = self
            .points
            .iter()
            .filter(|p| p.class == class && p.inst == inst && p.role.controllable())
            .map(|p| {
                format!(
                    "<DOI name=\"{}\"><DAI name=\"ctlModel\"><Val>{}</Val></DAI></DOI>",
                    p.name,
                    if p.sbo {
                        "sbo-with-normal-security"
                    } else {
                        "direct-with-normal-security"
                    }
                )
            })
            .collect();
        format!("<LN lnClass=\"{class}\" inst=\"{inst}\" prefix=\"\" lnType=\"T_{class}{inst}\">{dois}</LN>")
    }

    fn node_types(&self) -> String {
        let mut s = String::from("<LNodeType id=\"LLN0_T\" lnClass=\"LLN0\"/>");
        for (class, inst) in self.nodes() {
            let dos: String = self
                .points
                .iter()
                .filter(|p| p.class == class && p.inst == inst)
                .map(|p| {
                    format!(
                        "<DO name=\"{}\" type=\"{}\"/>",
                        p.name,
                        p.role.info().do_type
                    )
                })
                .collect();
            let _ = write!(
                s,
                "<LNodeType id=\"T_{class}{inst}\" lnClass=\"{class}\">{dos}</LNodeType>"
            );
        }
        s
    }

    /// The whole SCL document.
    pub fn scl(&self) -> String {
        let comm = self
            .goose
            .as_ref()
            .map(|g| self.communication(g))
            .unwrap_or_default();
        let lns: String = self.nodes().iter().map(|(c, i)| self.ln(c, *i)).collect();
        format!(
            "<?xml version=\"1.0\"?>\n<SCL xmlns=\"http://www.iec.ch/61850/2003/SCL\" version=\"2007\" revision=\"B\" release=\"4\"><Header id=\"ferrofire\"/>{comm}<IED name=\"{ied}\"><AccessPoint name=\"P1\"><Server><LDevice inst=\"{ld}\">{ln0}{lns}</LDevice></Server></AccessPoint></IED><DataTypeTemplates>{nt}{TYPES}</DataTypeTemplates></SCL>",
            ied = self.ied,
            ld = self.ld,
            ln0 = self.ln0(),
            nt = self.node_types(),
        )
    }

    /// Reject a model SCL cannot express: duplicate data object names in one node.
    pub fn check(&self) -> Result<(), String> {
        for (i, p) in self.points.iter().enumerate() {
            if self.points[..i]
                .iter()
                .any(|q| q.class == p.class && q.inst == p.inst && q.name == p.name)
            {
                return Err(format!("two points are named {}.{}", p.ln(), p.name));
            }
        }
        Ok(())
    }
}

/// Default data object name for the `n`th point of a role (1-based) in its node.
pub fn default_name(role: Role, n: usize) -> String {
    format!("{}{n}", role.info().stem)
}

/// `GGIO1` -> (`GGIO`, 1). Four capital letters and a number, as IEC 61850-7-2 names a node.
pub fn parse_ln(s: &str) -> Option<(String, u16)> {
    let (class, inst) = s.split_at(
        s.len()
            .checked_sub(s.chars().rev().take_while(char::is_ascii_digit).count())?,
    );
    let ok = class.len() == 4 && class.chars().all(|c| c.is_ascii_uppercase());
    let inst: u16 = inst.parse().ok().filter(|i| (1..=999).contains(i))?;
    ok.then(|| (class.to_string(), inst))
}

/// Fixed type templates: the four common data classes, the control parameters, the control models.
const TYPES: &str = concat!(
    "<DOType id=\"SPS_T\" cdc=\"SPS\"><DA name=\"stVal\" fc=\"ST\" bType=\"BOOLEAN\"/><DA name=\"q\" fc=\"ST\" bType=\"Quality\"/><DA name=\"t\" fc=\"ST\" bType=\"Timestamp\"/></DOType>",
    "<DOType id=\"MV_T\" cdc=\"MV\"><DA name=\"mag\" fc=\"MX\" bType=\"Struct\" type=\"AV_T\"/><DA name=\"q\" fc=\"MX\" bType=\"Quality\"/><DA name=\"t\" fc=\"MX\" bType=\"Timestamp\"/></DOType>",
    "<DOType id=\"SPC_T\" cdc=\"SPC\"><DA name=\"stVal\" fc=\"ST\" bType=\"BOOLEAN\"/><DA name=\"q\" fc=\"ST\" bType=\"Quality\"/><DA name=\"t\" fc=\"ST\" bType=\"Timestamp\"/><DA name=\"ctlModel\" fc=\"CF\" bType=\"Enum\" type=\"CtlModel_E\"/><DA name=\"Oper\" fc=\"CO\" bType=\"Struct\" type=\"OperSPC_T\"/></DOType>",
    "<DOType id=\"APC_T\" cdc=\"APC\"><DA name=\"mxVal\" fc=\"MX\" bType=\"Struct\" type=\"AV_T\"/><DA name=\"q\" fc=\"MX\" bType=\"Quality\"/><DA name=\"t\" fc=\"MX\" bType=\"Timestamp\"/><DA name=\"ctlModel\" fc=\"CF\" bType=\"Enum\" type=\"CtlModel_E\"/><DA name=\"Oper\" fc=\"CO\" bType=\"Struct\" type=\"OperAPC_T\"/></DOType>",
    "<DAType id=\"AV_T\"><BDA name=\"f\" bType=\"FLOAT32\"/></DAType>",
    "<DAType id=\"OperSPC_T\"><BDA name=\"ctlVal\" bType=\"BOOLEAN\"/><BDA name=\"origin\" bType=\"Struct\" type=\"Or_T\"/><BDA name=\"ctlNum\" bType=\"INT8U\"/><BDA name=\"T\" bType=\"Timestamp\"/><BDA name=\"Test\" bType=\"BOOLEAN\"/><BDA name=\"Check\" bType=\"Check\"/></DAType>",
    "<DAType id=\"OperAPC_T\"><BDA name=\"ctlVal\" bType=\"Struct\" type=\"AV_T\"/><BDA name=\"origin\" bType=\"Struct\" type=\"Or_T\"/><BDA name=\"ctlNum\" bType=\"INT8U\"/><BDA name=\"T\" bType=\"Timestamp\"/><BDA name=\"Test\" bType=\"BOOLEAN\"/><BDA name=\"Check\" bType=\"Check\"/></DAType>",
    "<DAType id=\"Or_T\"><BDA name=\"orCat\" bType=\"Enum\" type=\"OrCat_E\"/><BDA name=\"orIdent\" bType=\"Octet64\"/></DAType>",
    "<EnumType id=\"OrCat_E\"><EnumVal ord=\"0\">not-supported</EnumVal><EnumVal ord=\"1\">bay-control</EnumVal><EnumVal ord=\"2\">station-control</EnumVal><EnumVal ord=\"3\">remote-control</EnumVal><EnumVal ord=\"4\">automatic-bay</EnumVal><EnumVal ord=\"5\">automatic-station</EnumVal><EnumVal ord=\"6\">automatic-remote</EnumVal><EnumVal ord=\"7\">maintenance</EnumVal><EnumVal ord=\"8\">process</EnumVal></EnumType>",
    "<EnumType id=\"CtlModel_E\"><EnumVal ord=\"0\">status-only</EnumVal><EnumVal ord=\"1\">direct-with-normal-security</EnumVal><EnumVal ord=\"2\">sbo-with-normal-security</EnumVal><EnumVal ord=\"3\">direct-with-enhanced-security</EnumVal><EnumVal ord=\"4\">sbo-with-enhanced-security</EnumVal></EnumType>",
);

#[cfg(test)]
mod tests {
    use super::*;
    use iec61850_rs::server::Ied;

    fn pt(tag: &str, role: Role, class: &str, inst: u16, name: &str, sbo: bool) -> Point {
        Point {
            tag: tag.into(),
            role,
            class: class.into(),
            inst,
            name: name.into(),
            sbo,
        }
    }

    fn goose() -> Goose {
        Goose {
            interface: "lo".into(),
            mac: "01-0C-CD-01-00-00".into(),
            appid: 0x3E8,
            vlan_id: Some(1),
            vlan_priority: 4,
            min_ms: 4,
            max_ms: 1000,
            cb_name: "gcbAnalogValues".into(),
        }
    }

    fn model() -> Model {
        Model {
            ied: "IED1".into(),
            ld: "LD0".into(),
            points: vec![
                pt("a", Role::Ind, "GGIO", 1, "Ind1", false),
                pt("b", Role::AnIn, "GGIO", 1, "AnIn1", false),
                pt("c", Role::Spcso, "GGIO", 1, "SPCSO1", true),
                pt("d", Role::Apcso, "GGIO", 1, "APCSO1", false),
                pt("e", Role::AnIn, "MMXU", 2, "TotW", false),
            ],
            goose: Some(goose()),
        }
    }

    #[test]
    fn names_are_a_small_alphabet() {
        for ok in ["a", "IED_1", "GenericIO", &"x".repeat(32)] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "a b", "a\"b", "a<b", "a.b", "a$b", &"x".repeat(33)] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn logical_node_names() {
        assert_eq!(parse_ln("GGIO1"), Some(("GGIO".into(), 1)));
        assert_eq!(parse_ln("MMXU12"), Some(("MMXU".into(), 12)));
        assert_eq!(parse_ln("MMXU999"), Some(("MMXU".into(), 999)));
        for bad in [
            "", "GGIO", "GGIO0", "GGIO1000", "ggio1", "GGI1", "GGIOX1", "1", "GGIO-1",
        ] {
            assert_eq!(parse_ln(bad), None, "{bad}");
        }
    }

    #[test]
    fn roles_know_their_element_kind_and_control() {
        let all = [
            ("binary-input", Role::Ind, Kind::Binary, false),
            ("analog-input", Role::AnIn, Kind::Analog, false),
            ("binary-output", Role::Spcso, Kind::Binary, true),
            ("analog-output", Role::Apcso, Kind::Analog, true),
        ];
        for (el, role, kind, ctl) in all {
            assert_eq!(Role::from_element(el), Some(role));
            assert_eq!((role.kind(), role.controllable()), (kind, ctl));
        }
        assert_eq!(Role::from_element("coil"), None);
        assert_eq!(default_name(Role::Spcso, 3), "SPCSO3");
        assert_eq!(default_name(Role::AnIn, 1), "AnIn1");
    }

    #[test]
    fn the_generated_scl_loads_and_every_reference_exists() {
        let m = model();
        let ied = Ied::from_scl(&m.scl(), Some("IED1")).unwrap();
        let d = m.domain();
        assert_eq!(d, "IED1LD0");
        for p in &m.points {
            assert!(ied.value(&p.value_ref(&d)).is_some(), "{}", p.value_ref(&d));
            assert!(ied.value(&p.time_ref(&d)).is_some(), "{}", p.time_ref(&d));
            assert_eq!(
                ied.node_at(&p.control_ref(&d)).is_some(),
                p.role.controllable(),
                "{}",
                p.control_ref(&d)
            );
        }
        assert_eq!(m.points[2].value_ref(&d), "IED1LD0/GGIO1$ST$SPCSO1$stVal");
        assert_eq!(m.points[3].value_ref(&d), "IED1LD0/GGIO1$MX$APCSO1$mxVal$f");
        assert_eq!(m.points[1].time_ref(&d), "IED1LD0/GGIO1$MX$AnIn1$t");
        assert_eq!(m.points[2].control_ref(&d), "IED1LD0/GGIO1$CO$SPCSO1");
        assert_eq!(m.points[4].ln(), "MMXU2");
    }

    #[test]
    fn the_data_set_holds_every_value_and_quality_and_the_control_blocks_exist() {
        let m = model();
        let ied = Ied::from_scl(&m.scl(), Some("IED1")).unwrap();
        let members = ied.data_set("IED1LD0/LLN0$Events").unwrap();
        assert_eq!(members.members.len(), 2 * m.points.len());
        assert!(ied.block("IED1LD0/LLN0$RP$EventsRCB01").is_some());
        assert!(ied.block("IED1LD0/LLN0$GO$gcbAnalogValues").is_some());
        let plain = Model { goose: None, ..m };
        let ied = Ied::from_scl(&plain.scl(), Some("IED1")).unwrap();
        assert!(ied.block("IED1LD0/LLN0$GO$gcbAnalogValues").is_none());
        assert!(!plain.scl().contains("<Communication>"));
    }

    #[test]
    fn the_goose_address_is_in_the_scl() {
        let s = model().scl();
        for want in [
            "<P type=\"MAC-Address\">01-0C-CD-01-00-00</P>",
            "<P type=\"APPID\">03E8</P>",
            "<P type=\"VLAN-ID\">001</P>",
            "<P type=\"VLAN-PRIORITY\">4</P>",
            "<MinTime unit=\"s\" multiplier=\"m\">4</MinTime>",
            "<MaxTime unit=\"s\" multiplier=\"m\">1000</MaxTime>",
            "cbName=\"gcbAnalogValues\"",
        ] {
            assert!(s.contains(want), "{want}");
        }
        let mut g = goose();
        g.vlan_id = None;
        let s = Model {
            goose: Some(g),
            ..model()
        }
        .scl();
        assert!(!s.contains("VLAN"));
    }

    #[test]
    fn sbo_selects_the_control_model() {
        let s = model().scl();
        assert!(s.contains(
            "<DOI name=\"SPCSO1\"><DAI name=\"ctlModel\"><Val>sbo-with-normal-security</Val>"
        ));
        assert!(s.contains(
            "<DOI name=\"APCSO1\"><DAI name=\"ctlModel\"><Val>direct-with-normal-security</Val>"
        ));
        assert!(!s.contains("<DOI name=\"Ind1\""));
    }

    #[test]
    fn two_points_in_one_node_cannot_share_a_name() {
        assert!(model().check().is_ok());
        let mut m = model();
        m.points.push(pt("f", Role::Ind, "GGIO", 1, "Ind1", false));
        assert!(m.check().unwrap_err().contains("GGIO1.Ind1"));
        let mut m = model();
        m.points.push(pt("f", Role::Ind, "GGIO", 2, "Ind1", false));
        assert!(m.check().is_ok(), "the same name in another node is fine");
    }
}
