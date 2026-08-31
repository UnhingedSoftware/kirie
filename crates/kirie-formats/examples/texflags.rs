fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap_or_default();
        match kirie_formats::tex::Tex::parse(&bytes) {
            Ok(tex) => println!(
                "{path}: flags={:?} clamp_uvs={} nearest={}",
                tex.flags,
                tex.flags.clamp_uvs(),
                tex.flags.no_interpolation()
            ),
            Err(err) => println!("{path}: {err}"),
        }
    }
}
