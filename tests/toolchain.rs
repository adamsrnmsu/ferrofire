//! The toolchain is pinned once (rust-toolchain.toml); Cargo.toml's rust-version and the
//! ff-rust container must agree with it so local, CI and container builds are the same compiler.
use std::fs::read_to_string;

fn field(text: &str, key: &str) -> String {
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with(key))
        .unwrap_or_else(|| panic!("no {key}"));
    line.split('"')
        .nth(1)
        .unwrap_or_else(|| panic!("no quoted {key}"))
        .to_string()
}

#[test]
fn toolchain_pin_matches_rust_version_and_container() {
    let pin = field(
        &read_to_string("rust-toolchain.toml").expect("rust-toolchain.toml"),
        "channel",
    );
    assert!(pin.split('.').count() == 3, "pin an exact x.y.z, got {pin}");
    let msrv = field(&read_to_string("Cargo.toml").unwrap(), "rust-version");
    assert!(
        pin.starts_with(&msrv),
        "rust-version {msrv} must be a prefix of the pin {pin}"
    );
    let docker = read_to_string("docker/Dockerfile").unwrap();
    assert!(
        docker.contains(&format!("--default-toolchain {pin}")),
        "Dockerfile must install {pin}"
    );
}
