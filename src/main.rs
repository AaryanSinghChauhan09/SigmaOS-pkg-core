//! SigmaOS component binary: pkg-core

use sigmaos_pkg-core::Component;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    Component::new().operate()?;
    println!("sigmaos-pkg-core: ok");
    Ok(())
}
