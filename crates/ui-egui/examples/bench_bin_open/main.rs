//! Native benchmark for modular project open and first bin activation.
#[cfg(not(target_arch = "wasm32"))]
mod native;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    native::run()
}

#[cfg(target_arch = "wasm32")]
fn main() -> Result<(), &'static str> {
    Err("this benchmark requires native filesystem access")
}
