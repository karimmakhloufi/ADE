// Embeds the application icon into ade.exe on Windows (Explorer, taskbar, shortcuts).
fn main() {
    println!("cargo:rerun-if-changed=assets/ade.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/ade.ico")
            .set("FileDescription", "ADE - Agentic Development Environment")
            .set("ProductName", "ADE");
        if let Err(e) = res.compile() {
            println!("cargo:warning=icon not embedded: {e}");
        }
    }
}
