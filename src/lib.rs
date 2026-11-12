//! ferrofire: process (and, later, power-flow) simulator. `model` is the engine core; `simulink` is the shm
//! frontend that stands in for the Simulink-generated solver.

#![deny(unsafe_code)] // allowed only in `sys` and `simulink::ipc`

pub mod bennu;
pub mod datagram;
pub mod diff;
pub mod doctor;
#[cfg(feature = "elastic")]
pub mod elastic;
pub mod expr;
pub mod fd;
#[cfg(feature = "elastic")]
pub mod groundtruth;
pub mod gt;
pub mod hint;
pub mod log;
pub mod model;
pub mod power;
pub mod probe;
pub mod pull;
pub mod pypower;
pub mod scpi;
pub mod simulink;
pub mod sys;
pub mod top;
pub mod trace;
pub mod transport;
pub mod tui;

/// The waterway lock, used by the simulink adapter when no `--model` / `./model.yaml` is given.
pub const WATERWAY_YAML: &str = include_str!("../models/waterway.yaml");
