fn main() {
    // Ayarlar penceresinin standart bileşenleri koyu temada.
    let cfg = slint_build::CompilerConfiguration::new().with_style("fluent-dark".into());
    slint_build::compile_with_config("ui/dock.slint", cfg).expect("dock.slint derlenemedi");
}
