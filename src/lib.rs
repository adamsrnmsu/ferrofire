//! ferrofire: process (and, later, power-flow) simulator. `model` is the engine core; `simulink` is the shm
//! frontend that stands in for the Simulink-generated solver.

pub mod bennu;
pub mod expr;
pub mod model;
pub mod power;
pub mod pypower;
pub mod simulink;
pub mod transport;

/// The waterway lock, used by the simulink adapter when no `--model` / `./model.yaml` is given.
pub const WATERWAY_YAML: &str = include_str!("../models/waterway.yaml");

/// Print the usage text to stderr and exit 2, the code for a malformed command line.
pub fn usage_exit(text: &str) -> ! {
    eprintln!("{text}");
    std::process::exit(2)
}
