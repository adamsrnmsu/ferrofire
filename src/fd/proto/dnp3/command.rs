//! Control requests (SELECT, OPERATE, DIRECT OPERATE) and WRITE, answered the way bennu's `ServerCommandHandler` does.

use super::app::{self, iin, status, Control, ControlError, Request};
use super::outstation::{Device, Sink};
use std::time::Duration;

/// How long an OPERATE may follow its SELECT (opendnp3 `selectTimeout`).
const SELECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Re-assert NEED_TIME this long after a time write.
pub const TIME_SYNC_VALID: Duration = Duration::from_secs(30);

/// A SELECT waiting for its OPERATE: the request's sequence number, when it arrived, and its objects with status octets zeroed.
#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    pub seq: u8,
    pub at: Duration,
    pub key: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Select,
    Operate,
    Direct,
}

fn zeroed(objects: &[u8], cmds: &[Control]) -> Vec<u8> {
    let mut key = objects.to_vec();
    cmds.iter().for_each(|c| key[c.status_at] = 0);
    key
}

/// Whether this OPERATE matches the SELECT before it. The selection is used up either way.
fn matches_select(dev: &mut Device, req: &Request, key: &[u8], now: Duration) -> bool {
    dev.selection.take().is_some_and(|s| {
        s.seq.wrapping_add(1) & 0x0F == req.seq()
            && now.saturating_sub(s.at) <= SELECT_TIMEOUT
            && s.key == key
    })
}

fn select_status(dev: &Device, c: &Control) -> u8 {
    let known = match (c.group, c.var) {
        (12, 1) => dev.bin.contains_key(&c.index),
        (41, 3) => dev.ana.contains_key(&c.index),
        _ => true,
    };
    if known {
        status::SUCCESS
    } else {
        status::OUT_OF_RANGE
    }
}

fn crob_status(dev: &Device, sink: &mut dyn Sink, c: &Control, selected: bool) -> u8 {
    match dev.bin.get(&c.index) {
        None => status::OUT_OF_RANGE,
        Some(p) if p.sbo && !selected => status::NO_SELECT,
        Some(p) => {
            sink.binary(&p.tag, c.index, c.body[0] & 0x0F == 3);
            status::SUCCESS
        }
    }
}

fn analog_status(dev: &Device, sink: &mut dyn Sink, c: &Control, selected: bool) -> u8 {
    match dev.ana.get(&c.index) {
        None => status::OUT_OF_RANGE,
        Some(p) if p.sbo && !selected => status::NO_SELECT,
        Some(p) => {
            sink.analog(
                &p.tag,
                c.index,
                f32::from_le_bytes([c.body[0], c.body[1], c.body[2], c.body[3]]),
            );
            status::SUCCESS
        }
    }
}

fn operate_status(dev: &Device, sink: &mut dyn Sink, c: &Control, selected: bool) -> u8 {
    match (c.group, c.var) {
        (12, 1) => crob_status(dev, sink, c, selected),
        (41, 3) => analog_status(dev, sink, c, selected),
        // AnalogOutputInt16 / Int32 / Double64 are accepted and written nowhere (bennu quirk Q6)
        _ => status::SUCCESS,
    }
}

fn refusal(e: ControlError) -> u16 {
    match e {
        ControlError::Unknown => iin::UNKNOWN_OBJECT,
        _ => iin::PARAM_ERROR,
    }
}

/// Run a control request. `Ok` holds the request's objects with each status octet filled in; `Err` the indications of a
/// request refused as a whole.
pub fn control(
    dev: &mut Device,
    sink: &mut dyn Sink,
    req: &Request,
    mode: Mode,
    now: Duration,
) -> Result<Vec<u8>, u16> {
    let cmds = app::control_objects(req.objects).map_err(refusal)?;
    let key = zeroed(req.objects, &cmds);
    let selected = mode == Mode::Operate && matches_select(dev, req, &key, now);
    let statuses: Vec<u8> = cmds
        .iter()
        .map(|c| match mode {
            Mode::Select => select_status(dev, c),
            Mode::Operate if !selected => status::NO_SELECT,
            Mode::Operate => operate_status(dev, sink, c, true),
            Mode::Direct => operate_status(dev, sink, c, false),
        })
        .collect();
    if mode == Mode::Select && statuses.iter().all(|&s| s == status::SUCCESS) {
        dev.selection = Some(Selection {
            seq: req.seq(),
            at: now,
            key,
        });
    }
    let mut echo = req.objects.to_vec();
    cmds.iter()
        .zip(&statuses)
        .for_each(|(c, s)| echo[c.status_at] = *s);
    Ok(echo)
}

/// WRITE: clear the restart indication (g80v1 index 7) and set the time (g50v1). Anything else is a parameter error.
pub fn write(dev: &mut Device, objects: &[u8], now: Duration) -> Result<(), u16> {
    let mut rest = objects;
    while !rest.is_empty() {
        rest = match rest {
            [80, 1, 0x00, 7, 7, 0, tail @ ..] => {
                dev.restart = false;
                tail
            }
            [50, 1, 0x07, 1, _, _, _, _, _, _, tail @ ..] => {
                dev.set_time(now);
                tail
            }
            [80 | 50, ..] => return Err(iin::PARAM_ERROR),
            [g, ..] if app::KNOWN_GROUPS.contains(g) => return Err(iin::NO_FUNC),
            _ => return Err(iin::UNKNOWN_OBJECT),
        };
    }
    Ok(())
}
