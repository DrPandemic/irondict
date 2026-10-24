fn main() {
    // Slint's default style is `fluent` on every target, but fluent's
    // `ScrollView` sets `flickable.interactive: false`, which disables touch
    // drag-panning — so ListViews/ScrollViews don't scroll on a phone. Use the
    // `material` style on Android: native-feeling widgets with interactive
    // flickables. Desktop keeps `fluent`.
    let style = if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        "material"
    } else {
        "fluent"
    };
    let config = slint_build::CompilerConfiguration::new().with_style(style.into());
    slint_build::compile_with_config("ui/app.slint", config).expect("compiling ui/app.slint");
}
