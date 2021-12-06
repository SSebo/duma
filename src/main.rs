#[cfg(any(feature = "test", feature = "default"))]
use duma::cmd::run;

fn main() {
    #[cfg(any(feature = "test", feature = "default"))]
    match run() {
        Ok(_) => {
            println!(">>> here: ");
        }
        Err(e) => {
            eprintln!(">>> error: {}", e);
            std::process::exit(1);
        }
    }
}
