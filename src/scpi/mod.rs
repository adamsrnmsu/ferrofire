//! SCPI text handling, shared by the field device's SCPI server and the `ferrofire scpi` client.

pub mod client;
pub mod hislip;
pub mod rpc;
pub mod syntax;
pub mod transport;
