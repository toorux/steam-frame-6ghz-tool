fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winres::WindowsResource::new()
            .set_icon("assets/icon.ico")
            .compile()
            .expect("compile Windows application icon");
    }
}
