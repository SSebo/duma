use anyhow::Result;
use reqwest::header::{self, HeaderMap, HeaderValue};
use reqwest::{Client, Request};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::Semaphore;
use tokio::sync::{Mutex, RwLock};
use url::Url;

#[derive(Debug, Clone)]
pub struct Config {
    pub user_agent: String,
    pub resume: bool,
    pub headers: HeaderMap,
    pub file: String,
    pub timeout: u64,
    pub concurrent: bool,
    pub max_retries: u64,
    pub num_workers: usize,
    pub bytes_on_disk: Option<u64>,
    pub chunk_offsets: Option<Vec<(u64, u64)>>,
    pub chunk_size: u64,
    pub quiet_mode: bool,
}

#[allow(unused_variables)]
pub trait EventsHandler {
    fn on_resume_download(&mut self, bytes_on_disk: u64) {}

    fn on_headers(&mut self, headers: HeaderMap) {}

    fn on_content(&mut self, content: &[u8]) -> Result<()> {
        Ok(())
    }

    fn on_concurrent_content(&mut self, content: (u64, u64, &[u8])) -> Result<()> {
        Ok(())
    }

    fn on_content_length(&mut self, ct_len: u64) {}

    #[cfg(feature = "ftp")]
    fn on_ftp_content_length(&mut self, ct_len: Option<u64>) {}

    fn on_success_status(&self) {}

    fn on_failure_status(&self, status_code: i32) {}

    fn on_finish(&mut self) {}

    fn on_max_retries(&mut self) {}

    fn on_server_supports_resume(&mut self) {}
}

pub struct HttpDownload {
    url: Url,
    hooks: Arc<Mutex<Vec<Box<dyn EventsHandler + Send>>>>,
    conf: Config,
    retries: Arc<Mutex<u64>>,
    client: Client,
    is_cancel: Arc<RwLock<bool>>,
}

impl fmt::Debug for HttpDownload {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "HttpDownload url: {}", self.url)
    }
}

impl HttpDownload {
    pub fn new(url: Url, conf: Config) -> HttpDownload {
        HttpDownload {
            url,
            hooks: Arc::new(Mutex::new(Vec::new())),
            conf,
            retries: Arc::new(Mutex::new(0)),
            client: Client::new(),
            is_cancel: Arc::new(RwLock::new(false)),
        }
    }

    pub async fn cancel(&mut self) {
        let mut guard = self.is_cancel.write().await;
        *guard = true;
    }

    pub async fn download(&mut self) -> Result<()> {
        log::info!("download conf {:?}", self.conf);
        let req = self
            .client
            .get(self.url.as_ref())
            .timeout(Duration::from_secs(self.conf.timeout))
            .headers(self.conf.headers.clone())
            .header(
                header::USER_AGENT,
                HeaderValue::from_str(&self.conf.user_agent)?,
            );
        log::info!("req {:?}", req);
        let resp = req.send().await?;
        let headers = resp.headers();
        log::info!("resp headers {:?}", headers);

        let server_supports_bytes = match headers.get(header::ACCEPT_RANGES) {
            Some(val) => val == "bytes",
            None => false,
        };

        if server_supports_bytes && self.conf.headers.contains_key(header::RANGE) {
            if self.conf.concurrent {
                self.conf.headers.remove(header::RANGE);
            }
            run_hooks(
                self.hooks.clone(),
                Box::new(|hook| hook.on_server_supports_resume()),
            )
            .await;
        }

        let req = self
            .client
            .get(self.url.as_ref())
            .timeout(Duration::from_secs(self.conf.timeout))
            .headers(self.conf.headers.clone())
            .build()?;

        let headers_clone = headers.to_owned();
        run_hooks(
            self.hooks.clone(),
            Box::new(move |hook| hook.on_headers(headers_clone.clone())),
        )
        .await;

        if server_supports_bytes
            && self.conf.concurrent
            && headers.contains_key(header::CONTENT_LENGTH)
        {
            self.concurrent_download(req, headers.get(header::CONTENT_LENGTH).unwrap())
                .await?;
        } else {
            self.singlethread_download(req).await?;
        }

        run_hooks(self.hooks.clone(), Box::new(|hook| hook.on_finish())).await;

        Ok(())
    }

    pub async fn events_hook<E: EventsHandler + 'static + Send>(
        &mut self,
        hk: E,
    ) -> &mut HttpDownload {
        let hooks = self.hooks.clone();
        let mut guard = hooks.lock().await;
        (*guard).push(Box::new(hk));
        self
    }

    async fn singlethread_download(&mut self, req: Request) -> Result<()> {
        let mut resp = self.client.execute(req).await?;
        while let Some(bytes) = resp.chunk().await? {
            if *(self.is_cancel.read().await) {
                break;
            }
            log::info!("single thread got chunk");
            self.send_content(&bytes.to_vec()).await?;
        }
        Ok(())
    }

    pub async fn concurrent_download(&mut self, req: Request, ct_val: &HeaderValue) -> Result<()> {
        let (data_tx, mut data_rx) = mpsc::unbounded_channel();
        let (errors_tx, mut errors_rx) = mpsc::unbounded_channel();
        let ct_len = ct_val.to_str()?.parse::<u64>()?;
        let chunk_offsets = self
            .conf
            .chunk_offsets
            .clone()
            .unwrap_or_else(|| self.get_chunk_offsets(ct_len, self.conf.chunk_size));
        let sem = Arc::new(Semaphore::new(self.conf.num_workers));
        let is_cancel = self.is_cancel.clone();
        do_concurrent_download(
            req.try_clone().unwrap(),
            chunk_offsets,
            data_tx.clone(),
            errors_tx.clone(),
            sem.clone(),
            is_cancel,
        );

        let mut count = self.conf.bytes_on_disk.unwrap_or(0);
        let bytes_on_disk = count;

        loop {
            if *(self.is_cancel.read().await) {
                break;
            }
            if count == ct_len + bytes_on_disk {
                break;
            }
            if let Some((byte_count, offset, buf)) = data_rx.recv().await {
                count += byte_count;
                if let Err(e) = try_run_hooks(
                    self.hooks.clone(),
                    Box::new(move |hk| hk.on_concurrent_content((byte_count, offset, &buf))),
                )
                .await
                {
                    log::error!("concurrent_content error {:?}", e);
                    return Err(e);
                }
            }
            let max_retry = self
                .do_retry_download(
                    req.try_clone().unwrap(),
                    data_tx.clone(),
                    &mut errors_rx,
                    errors_tx.clone(),
                    sem.clone(),
                )
                .await;
            if max_retry {
                break;
            }
        }
        Ok(())
    }

    async fn do_retry_download(
        &self,
        req: Request,
        data_tx: UnboundedSender<(u64, u64, Vec<u8>)>,
        errors_rx: &mut UnboundedReceiver<(u64, u64)>,
        errors_tx: UnboundedSender<(u64, u64)>,
        sem: Arc<Semaphore>,
    ) -> bool {
        if let Ok(Some(offsets)) =
            tokio::time::timeout(tokio::time::Duration::from_micros(1), errors_rx.recv()).await
        {
            let mut retry_guard = self.retries.lock().await;
            if *retry_guard > self.conf.max_retries {
                run_hooks(self.hooks.clone(), Box::new(|hk| hk.on_max_retries())).await;
                return true;
            }
            log::error!("timeout retry {} for offset {:?}", *retry_guard, offsets);
            *retry_guard += 1;
            let permit = sem.clone().acquire_owned().await;
            tokio::spawn(async move {
                let _permit = permit;
                download_chunk(req, offsets, data_tx, errors_tx).await
            });
        }
        return false;
    }

    fn get_chunk_offsets(&self, ct_len: u64, chunk_size: u64) -> Vec<(u64, u64)> {
        log::info!("get_chunk_offsets {} {}", ct_len, chunk_size);
        let no_of_chunks = ct_len / chunk_size;
        let mut sizes = Vec::new();

        for chunk in 0..no_of_chunks {
            let bound = if chunk == no_of_chunks - 1 {
                ct_len
            } else {
                ((chunk + 1) * chunk_size) - 1
            };
            sizes.push((chunk * chunk_size, bound));
        }
        if sizes.is_empty() {
            sizes.push((0, ct_len));
        }

        sizes
    }

    async fn send_content(&mut self, contents: &[u8]) -> Result<()> {
        let mut hooks = self.hooks.lock().await;
        (*hooks)
            .iter_mut()
            .try_for_each(|hk| hk.on_content(contents))
    }
}

fn do_concurrent_download(
    req: Request,
    chunk_offsets: Vec<(u64, u64)>,
    data_tx: UnboundedSender<(u64, u64, Vec<u8>)>,
    errors_tx: UnboundedSender<(u64, u64)>,
    sem: Arc<Semaphore>,
    is_cancel: Arc<RwLock<bool>>,
) {
    tokio::spawn(async move {
        for offsets in chunk_offsets {
            let dtx = data_tx.clone();
            let etx = errors_tx.clone();
            let r = req.try_clone().unwrap();
            let is_cancel = is_cancel.read().await;
            if *is_cancel {
                break;
            }
            let permit = sem.clone().acquire_owned().await;
            tokio::spawn(async move {
                let _permit = permit;
                download_chunk(r, offsets, dtx.clone(), etx).await
            });
        }
    });
}

async fn download_chunk(
    req: Request,
    offsets: (u64, u64),
    sender: mpsc::UnboundedSender<(u64, u64, Vec<u8>)>,
    errors: mpsc::UnboundedSender<(u64, u64)>,
) {
    async fn inner(
        mut req: Request,
        offsets: (u64, u64),
        sender: mpsc::UnboundedSender<(u64, u64, Vec<u8>)>,
        start_offset: &mut u64,
    ) -> Result<()> {
        let byte_range = format!("bytes={}-{}", offsets.0, offsets.1);
        let headers = req.headers_mut();
        headers.insert(header::RANGE, HeaderValue::from_str(&byte_range)?);
        headers.insert(header::ACCEPT, HeaderValue::from_str("*/*")?);
        headers.insert(header::CONNECTION, HeaderValue::from_str("keep-alive")?);
        log::info!("chunk header {:?}", headers);
        let mut res = Client::new().execute(req).await?;
        let chunk_sz = offsets.1 - offsets.0;
        let mut cnt = 0u64;

        while let Some(bytes) = res.chunk().await? {
            let byte_count = bytes.len();
            cnt += byte_count as u64;
            if byte_count != 0 {
                sender.send((byte_count as u64, *start_offset, bytes.to_vec()))?;
                *start_offset += byte_count as u64;
                log::info!("chunk stream {}", byte_count);
            } else {
                break;
            }
            if cnt == (chunk_sz + 1) {
                break;
            }
        }

        Ok(())
    }
    let mut start_offset = offsets.0;
    let end_offset = offsets.1;
    match inner(req, offsets, sender, &mut start_offset).await {
        Ok(_) => {}
        Err(_) => {
            if errors.send((start_offset, end_offset)).is_ok() {};
            {}
        }
    }
}

pub async fn run_hooks(
    hooks: Arc<Mutex<Vec<Box<dyn EventsHandler + Send>>>>,
    f: Box<dyn FnMut(&mut Box<(dyn EventsHandler + Send + 'static)>) + Send>,
) {
    let mut hooks_guard = hooks.lock().await;
    (*hooks_guard).iter_mut().for_each(f);
    drop(hooks_guard);
}

pub async fn try_run_hooks(
    hooks: Arc<Mutex<Vec<Box<dyn EventsHandler + Send>>>>,
    f: Box<dyn FnMut(&mut Box<(dyn EventsHandler + Send + 'static)>) -> Result<()> + Send>,
) -> Result<()> {
    let mut hooks_guard = hooks.lock().await;
    (*hooks_guard).iter_mut().try_for_each(f)
}
