#[cfg(feature = "default")]
use crate::bar::create_progress_bar;
#[cfg(feature = "default")]
pub use indicatif::ProgressBar;

pub trait Progress: Send + Sync {
    fn new(msg: &str, length: Option<u64>) -> Self;

    fn inc(&self, delta: u64);

    fn finish(&self, status: u8, path: Option<String>);
}

#[cfg(feature = "default")]
impl Progress for ProgressBar {
    fn new(msg: &str, length: Option<u64>) -> Self {
        create_progress_bar(msg, length)
    }

    fn inc(&self, delta: u64) {
        ProgressBar::inc(self, delta);
    }

    fn finish(&self, _status: u8, _path: Option<String>) {
        ProgressBar::finish(self);
    }
}

pub struct ProgressCallback {}

impl Progress for ProgressCallback {
    #[allow(unused_variables)]
    fn new(msg: &str, length: Option<u64>) -> Self {
        ProgressCallback {}
    }

    #[allow(unused_variables)]
    fn inc(&self, delta: u64) {
        println!("inc {}", delta);
    }

    #[allow(unused_variables)]
    fn finish(&self, status: u8, path: Option<String>) {
        println!("finish {} {:?}", status, path)
    }
}
