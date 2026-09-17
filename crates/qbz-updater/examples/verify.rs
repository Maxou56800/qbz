//! Verify a downloaded release without installing or launching it.
fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        eprintln!("usage: verify ASSET SIGNATURE_FILE");
        std::process::exit(2);
    }
    let result = std::fs::read_to_string(&args[1])
        .map_err(|e| e.to_string())
        .and_then(|sig| qbz_updater::install::verify(std::path::Path::new(&args[0]), &sig));
    match result {
        Ok(()) => println!("QBZ release signature verified"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
