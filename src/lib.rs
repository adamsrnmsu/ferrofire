//! ferrofire: process (and, later, power-flow) simulator. `model` is the engine core; `simulink` is the shm
//! frontend that stands in for the Simulink-generated solver.

pub mod bennu;
pub mod datagram;
pub mod expr;
pub mod fd;
pub mod hint;
pub mod model;
pub mod power;
pub mod pull;
pub mod pypower;
pub mod simulink;
pub mod transport;

/// The waterway lock, used by the simulink adapter when no `--model` / `./model.yaml` is given.
pub const WATERWAY_YAML: &str = include_str!("../models/waterway.yaml");
