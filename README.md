# ferrofire

**The fire phēnix rises from.**

ferrofire is the **F**ield-to-**I**nfrastructure **R**untime **E**ngine: a Rust
simulator for the physical side of [phēnix](https://github.com/sandialabs/sceptre-phenix)
/ SCEPTRE cyber ranges. It models continuous processes (tanks, pipes, valves, pumps,
locks) and power systems (AC power flow in the style of PYPOWER) from plain YAML model
files, and serves them to the range's field devices, with no MATLAB, Simulink or
PowerWorld required for simple processes.

## The name

- **Ferro** is iron, the metal that rusts. ferrofire is written in Rust.
- **Fire** is the forge, where raw iron becomes something useful.
- **phēnix** is named for the phoenix, which rises from fire. ferrofire is the
  process that a phēnix range rises from: the simulated plant behind the RTUs,
  SCADA and HMI.

## Status

Early development. The design spec covers the model format, the solvers, the bennu
provider protocol and the SCEPTRE integration routes; code lands here as the phases
complete.

## License

MIT. Code ported from PYPOWER keeps PYPOWER's BSD-3 notice (PSERC) in a NOTICE file.
