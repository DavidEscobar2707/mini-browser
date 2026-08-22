#[cfg(windows)]
fn main() {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico");
    res.compile()
        .expect("failed to compile Windows resources (assets/icon.ico)");
}

#[cfg(not(windows))]
fn main() {}
