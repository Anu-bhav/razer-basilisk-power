fn main() {
    println!("cargo:rerun-if-changed=assets/basilisk-power.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/basilisk-power.ico")
            .set("ProductName", "Basilisk Power")
            .set("FileDescription", "Basilisk Power")
            .set("CompanyName", "Anu-bhav");
        res.compile().expect("failed to embed Windows resources");
    }
}
