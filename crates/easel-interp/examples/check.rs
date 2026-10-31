fn main() {
    for path in std::env::args().skip(1) {
        let source = std::fs::read_to_string(&path).unwrap();
        match easel_interp::interpret(&source) {
            Ok(program) => println!("OK   {path}: {}", program.entry),
            Err(diags) => {
                println!("FAIL {path}:");
                for d in diags.iter().take(6) {
                    println!("       {d}");
                }
                if diags.len() > 6 {
                    println!("       … {} more", diags.len() - 6);
                }
            }
        }
    }
}
