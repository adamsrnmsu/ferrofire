//! Answering a READ: classify each object header, select the events it asks for, gather the static points, and pack both into
//! fragments of at most `MAX_APDU` bytes. Events come first, then static data, as opendnp3 writes them.

use super::app::{self, iin, Header, Range};
use super::db::{analog_object, binary_static, Events, Typ};
use super::outstation::Device;
use std::collections::VecDeque;

/// One run of objects with a shared header.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// Events (qualifier 0x28), oldest first: `(event id, index prefix + object bytes)`. Binary variation 3 needs a CTO first.
    Events {
        group: u8,
        var: u8,
        items: Vec<(u64, Vec<u8>)>,
    },
    /// Consecutive indices from `start` (qualifier 0x00, or 0x01 when `wide`), one entry per point. opendnp3 writes two-octet
    /// ranges for a type whenever that type has a point above index 255, whatever range the header covers.
    Static {
        group: u8,
        var: u8,
        start: u16,
        wide: bool,
        items: Vec<Vec<u8>>,
    },
}

#[derive(Debug, PartialEq)]
pub struct Plan {
    pub blocks: VecDeque<Block>,
    /// Indications this answer adds: PARAMETER_ERROR when some asked-for index does not exist.
    pub iin: u16,
}

#[derive(Debug, PartialEq, Clone)]
enum Sel {
    All,
    Range(u32, u32),
    List(Vec<u32>),
}

#[derive(Debug, PartialEq)]
enum Action {
    Nothing,
    Class0,
    /// Select events: type (None = every type), variation (0 = default), most to select.
    Events(Option<Typ>, u8, usize),
    Static(Typ, u8, Sel),
}

/// Groups opendnp3 knows and the outstation answers with nothing (no such points here).
const EMPTY_GROUPS: &[u8] = &[3, 4, 10, 11, 20, 21, 22, 23, 40, 42];
/// Groups that exist but cannot be read.
const UNREADABLE_GROUPS: &[u8] = &[12, 41, 50, 51, 52, 80];

fn limit(range: &Range) -> Result<usize, u16> {
    match range {
        Range::All => Ok(usize::MAX),
        Range::Count(0) => Err(iin::PARAM_ERROR),
        Range::Count(n) => Ok(*n as usize),
        _ => Err(iin::NO_FUNC),
    }
}

fn static_sel(range: &Range) -> Result<Sel, u16> {
    match range {
        Range::All => Ok(Sel::All),
        Range::StartStop(a, b) => Ok(Sel::Range(*a, *b)),
        Range::Indices(v) => Ok(Sel::List(v.clone())),
        Range::Count(_) => Err(iin::NO_FUNC),
    }
}

fn class_action(var: u8, range: &Range) -> Result<Action, u16> {
    match var {
        1 => Ok(Action::Class0),
        2 => Ok(Action::Events(None, 0, limit(range)?)),
        3 | 4 => Ok(Action::Nothing),
        _ => Err(iin::UNKNOWN_OBJECT),
    }
}

/// Objects that exist in opendnp3 but have no points here, or cannot be read; anything else is unknown.
fn other_action(group: u8, var: u8) -> Result<Action, u16> {
    if EMPTY_GROUPS.contains(&group) && var <= 10 {
        Ok(Action::Nothing)
    } else if UNREADABLE_GROUPS.contains(&group) {
        Err(iin::NO_FUNC)
    } else {
        Err(iin::UNKNOWN_OBJECT)
    }
}

fn static_action(typ: Typ, var: u8, range: &Range) -> Result<Action, u16> {
    Ok(Action::Static(typ, var, static_sel(range)?))
}

fn event_action(typ: Typ, var: u8, range: &Range) -> Result<Action, u16> {
    Ok(Action::Events(Some(typ), var, limit(range)?))
}

fn classify(h: &Header) -> Result<Action, u16> {
    let (g, v) = (h.group, h.var);
    match (g, v) {
        (60, _) => class_action(v, &h.range),
        (1, 0..=2) => static_action(Typ::Binary, v, &h.range),
        (30, 0..=6) => static_action(Typ::Analog, v, &h.range),
        (2, 0..=3) => event_action(Typ::Binary, v, &h.range),
        (32, 0..=8) => event_action(Typ::Analog, v, &h.range),
        _ => other_action(g, v),
    }
}

/// Parse a READ request's objects into actions. `Err` carries the indications of a request that is refused as a whole.
fn actions(objects: &[u8]) -> Result<Vec<Action>, u16> {
    let headers = app::read_headers(objects).map_err(|_| iin::PARAM_ERROR)?;
    let (mut ok, mut bad) = (vec![], 0);
    for h in &headers {
        match classify(h) {
            Ok(a) => ok.push(a),
            Err(e) => bad |= e,
        }
    }
    if bad == 0 {
        Ok(ok)
    } else {
        Err(bad)
    }
}

/// Class 0 reads every type: add the ones no loader has yet.
fn add_class0(out: &mut Vec<(Typ, u8, Sel)>) {
    for t in [Typ::Binary, Typ::Analog] {
        if !out.iter().any(|l| l.0 == t) {
            out.push((t, 0, Sel::All));
        }
    }
}

/// An explicit header replaces a whole-type loader of its type (keeping its place), else adds a loader.
fn add_static(out: &mut Vec<(Typ, u8, Sel)>, typ: Typ, var: u8, sel: &Sel) {
    match out
        .iter_mut()
        .find(|l| l.0 == typ && l.2 == Sel::All && *sel == Sel::All)
    {
        Some(l) => l.1 = var,
        None => out.push((typ, var, sel.clone())),
    }
}

/// Static loaders in request order.
fn loaders(acts: &[Action]) -> Vec<(Typ, u8, Sel)> {
    let mut out = vec![];
    for a in acts {
        match a {
            Action::Class0 => add_class0(&mut out),
            Action::Static(t, v, sel) => add_static(&mut out, *t, *v, sel),
            _ => {}
        }
    }
    out
}

/// Every point of a type as `(index, variation, object bytes)`; `var` 0 means each point's own default.
fn all_points(dev: &Device, typ: Typ, var: u8) -> Vec<(u16, u8, Vec<u8>)> {
    let pick = |default: u8| if var == 0 { default } else { var };
    match typ {
        Typ::Binary => dev
            .bin
            .iter()
            .map(|(i, p)| {
                (
                    *i,
                    pick(p.default_var()),
                    binary_static(pick(p.default_var()), p.flags),
                )
            })
            .collect(),
        Typ::Analog => dev
            .ana
            .iter()
            .map(|(i, p)| {
                (
                    *i,
                    pick(p.default_var()),
                    analog_object(pick(p.default_var()), p.value, p.flags, None, false),
                )
            })
            .collect(),
    }
}

/// How many distinct points a selection names, and whether an index is in it.
fn wanted(sel: &Sel, i: u16) -> bool {
    match sel {
        Sel::All => true,
        Sel::Range(a, b) => (*a..=*b).contains(&u32::from(i)),
        Sel::List(l) => l.contains(&u32::from(i)),
    }
}

fn asked(sel: &Sel, found: usize) -> usize {
    match sel {
        Sel::All => found,
        Sel::Range(a, b) => (*b - *a + 1) as usize,
        Sel::List(l) => {
            let mut s = l.clone();
            s.sort_unstable();
            s.dedup();
            s.len()
        }
    }
}

/// The points a loader addresses, and whether some asked-for index is missing.
fn gather(dev: &Device, typ: Typ, var: u8, sel: &Sel) -> (Vec<(u16, u8, Vec<u8>)>, bool) {
    if matches!(sel, Sel::Range(a, b) if b < a) {
        return (vec![], true);
    }
    let found: Vec<_> = all_points(dev, typ, var)
        .into_iter()
        .filter(|(i, _, _)| wanted(sel, *i))
        .collect();
    let missing = found.len() != asked(sel, found.len());
    (found, missing)
}

/// Group consecutive indices that share a variation into blocks.
fn runs(typ: Typ, wide: bool, points: Vec<(u16, u8, Vec<u8>)>) -> Vec<Block> {
    let mut out: Vec<Block> = vec![];
    let mut last: Option<(u16, u8)> = None;
    for (idx, var, bytes) in points {
        let extend = matches!(last, Some((i, v)) if i.checked_add(1) == Some(idx) && v == var);
        match (extend, out.last_mut()) {
            (true, Some(Block::Static { items, .. })) => items.push(bytes),
            _ => out.push(Block::Static {
                group: if typ == Typ::Binary { 1 } else { 30 },
                var,
                start: idx,
                wide,
                items: vec![bytes],
            }),
        }
        last = Some((idx, var));
    }
    out
}

/// Events that are selected, in the order they happened, as blocks of one type and variation.
fn event_blocks(events: &Events) -> Vec<Block> {
    let mut out: Vec<Block> = vec![];
    for (e, var) in events.selected() {
        let (g, item) = (e.typ().group(), (e.id, e.encode(var)));
        match out.last_mut() {
            Some(Block::Events {
                group,
                var: v,
                items,
            }) if *group == g && *v == var => items.push(item),
            _ => out.push(Block::Events {
                group: g,
                var,
                items: vec![item],
            }),
        }
    }
    out
}

/// Plan the answer to a READ and select the events it asks for. A request that is refused as a whole selects nothing.
pub fn plan(dev: &mut Device, objects: &[u8]) -> Result<Plan, u16> {
    let acts = actions(objects)?;
    for a in &acts {
        if let Action::Events(typ, var, n) = a {
            dev.events.select(*typ, *var, *n);
        }
    }
    let mut blocks: VecDeque<Block> = event_blocks(&dev.events).into();
    let mut flags = 0;
    for (typ, var, sel) in loaders(&acts) {
        let (points, missing) = gather(dev, typ, var, &sel);
        if missing {
            flags |= iin::PARAM_ERROR;
        }
        blocks.extend(runs(typ, dev.max_index(typ) > 255, points));
    }
    Ok(Plan { blocks, iin: flags })
}

const CTO: [u8; 10] = [0x33, 0x02, 0x07, 0x01, 0, 0, 0, 0, 0, 0];

fn header_len(block: &Block) -> usize {
    match block {
        Block::Events {
            group: 2, var: 3, ..
        } => CTO.len() + 5,
        Block::Events { .. } => 5,
        Block::Static { wide, .. } => 3 + if *wide { 4 } else { 2 },
    }
}

fn len(block: &Block) -> usize {
    match block {
        Block::Events { items, .. } => items.len(),
        Block::Static { items, .. } => items.len(),
    }
}

fn item_sizes(block: &Block) -> Box<dyn Iterator<Item = usize> + '_> {
    match block {
        Block::Events { items, .. } => Box::new(items.iter().map(|i| i.1.len())),
        Block::Static { items, .. } => Box::new(items.iter().map(Vec::len)),
    }
}

/// How many leading entries of `block` fit in `space` bytes with their header (binary variation 1 packs eight to the octet).
fn fit(block: &Block, space: usize) -> usize {
    let packed = matches!(
        block,
        Block::Static {
            group: 1,
            var: 1,
            ..
        }
    );
    let (mut used, mut k) = (0, 0);
    for (i, size) in item_sizes(block).enumerate() {
        used += size;
        let data = if packed { (i + 1).div_ceil(8) } else { used };
        if header_len(block) + data > space {
            break;
        }
        k = i + 1;
    }
    k
}

/// The first `k` entries of `block` as bytes (events: the ids too), and the rest of the block.
fn emit(block: Block, k: usize) -> (Vec<u8>, Vec<u64>, Option<Block>) {
    let whole = k == len(&block);
    match block {
        Block::Events {
            group,
            var,
            mut items,
        } => {
            let rest = items.split_off(k);
            let mut out = if (group, var) == (2, 3) {
                CTO.to_vec()
            } else {
                vec![]
            };
            out.extend_from_slice(&[group, var, 0x28]);
            out.extend_from_slice(&(k as u16).to_le_bytes());
            let ids = items.iter().map(|i| i.0).collect();
            items.iter().for_each(|i| out.extend_from_slice(&i.1));
            (
                out,
                ids,
                (!whole).then_some(Block::Events {
                    group,
                    var,
                    items: rest,
                }),
            )
        }
        Block::Static {
            group,
            var,
            start,
            wide,
            mut items,
        } => {
            let rest = items.split_off(k);
            let stop = usize::from(start) + k - 1;
            let mut out = vec![group, var, u8::from(wide)];
            let put = |out: &mut Vec<u8>, v: usize| {
                out.push(v as u8);
                if wide {
                    out.push((v >> 8) as u8);
                }
            };
            put(&mut out, usize::from(start));
            put(&mut out, stop);
            if (group, var) == (1, 1) {
                let mut bytes = vec![0u8; k.div_ceil(8)];
                items
                    .iter()
                    .enumerate()
                    .for_each(|(i, e)| bytes[i / 8] |= (e[0] & 1) << (i % 8));
                out.extend(bytes);
            } else {
                items.iter().for_each(|e| out.extend_from_slice(e));
            }
            let next = start.wrapping_add(k as u16);
            (
                out,
                vec![],
                (!whole).then_some(Block::Static {
                    group,
                    var,
                    start: next,
                    wide,
                    items: rest,
                }),
            )
        }
    }
}

/// Fill one fragment's object area (at most `space` bytes) from the front of `blocks`. Returns the bytes and the ids of the
/// events written.
pub fn pack(blocks: &mut VecDeque<Block>, mut space: usize) -> (Vec<u8>, Vec<u64>) {
    let (mut out, mut ids) = (vec![], vec![]);
    while let Some(block) = blocks.pop_front() {
        let k = fit(&block, space);
        if k == 0 {
            blocks.push_front(block);
            break;
        }
        let (bytes, written, rest) = emit(block, k);
        space -= bytes.len();
        out.extend(bytes);
        ids.extend(written);
        if let Some(r) = rest {
            blocks.push_front(r);
            break;
        }
    }
    (out, ids)
}
