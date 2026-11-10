//! ferrofire: process (and, later, power-flow) simulator. `model` is the engine core; `simulink` is the shm
//! frontend that stands in for the Simulink-generated solver.

#![deny(unsafe_code)] // allowed only in `sys` and `simulink::ipc`

pub mod bennu;
pub mod datagram;
pub mod diff;
pub mod expr;
pub mod hint;
pub mod log;
pub mod model;
pub mod power;
pub mod probe;
pub mod pull;
pub mod pypower;
pub mod simulink;
pub mod sys;
pub mod trace;
pub mod transport;

/// The waterway lock, used by the simulink adapter when no `--model` / `./model.yaml` is given.
pub const WATERWAY_YAML: &str = include_str!("../models/waterway.yaml");
