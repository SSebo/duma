use crate::core::EventsHandler;
use anyhow::{format_err, Result};
use ftp::FtpStream;
use std::cell::RefCell;
#[cfg(feature = "enable-ftp")]
use std::io::Read;
use url::Url;

pub struct FtpDownload {
    url: Url,
    hooks: Vec<RefCell<Box<dyn EventsHandler>>>,
}

impl FtpDownload {
    pub fn new(url: Url) -> Self {
        Self {
            url,
            hooks: Vec::new(),
        }
    }

    pub fn download(&mut self) -> Result<()> {
        let ftp_server = format!(
            "{}:{}",
            self.url
                .host_str()
                .ok_or_else(|| format_err!("failed to parse hostname from url: {}", self.url))?,
            self.url
                .port_or_known_default()
                .ok_or_else(|| format_err!("failed to parse port from url: {}", self.url))?,
        );
        let username = if self.url.username().is_empty() {
            "anonymous"
        } else {
            self.url.username()
        };
        let password = self.url.password().unwrap_or("anonymous");

        let mut path_segments: Vec<&str> = self
            .url
            .path_segments()
            .ok_or_else(|| format_err!("failed to get url path segments: {}", self.url))?
            .collect();
        let ftp_fname = path_segments
            .pop()
            .ok_or_else(|| format_err!("got empty path segments from url: {}", self.url))?;

        let mut conn = FtpStream::connect(ftp_server)?;
        conn.login(username, password)?;
        for path in &path_segments {
            conn.cwd(path)?;
        }
        let ct_len = conn.size(ftp_fname)?;
        let mut reader = conn.get(ftp_fname)?;

        for hook in &self.hooks {
            let ct_len = ct_len.map(|x| x as u64);
            hook.borrow_mut().on_ftp_content_length(ct_len);
        }

        loop {
            let mut buffer = vec![0; 2048usize];
            let bcount = reader.read(&mut buffer[..])?;
            buffer.truncate(bcount);
            if !buffer.is_empty() {
                self.send_content(buffer.as_slice())?;
            } else {
                break;
            }
        }

        for hook in &self.hooks {
            hook.borrow_mut().on_finish();
        }

        Ok(())
    }

    fn send_content(&self, contents: &[u8]) -> Result<()> {
        for hk in &self.hooks {
            hk.borrow_mut().on_content(contents)?;
        }
        Ok(())
    }
    pub fn events_hook<E: EventsHandler + 'static>(&mut self, hk: E) -> &mut FtpDownload {
        self.hooks.push(RefCell::new(Box::new(hk)));
        self
    }
}
