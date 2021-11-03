#[cfg(feature = "default")]
use duma::cmd::run;

fn main() {
    #[cfg(feature = "default")]
    match run() {
        Ok(_) => {}
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}
