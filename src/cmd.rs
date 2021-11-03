use crate::core::Config;
#[cfg(feature = "enable-ftp")]
use crate::download::ftp_download;
use crate::download::print_headers;
use crate::download::{
    calc_bytes_on_disk, gen_filename, get_resume_chunk_offsets, http_download, prep_headers,
    request_headers_from_server,
};
use crate::utils;
use anyhow::{format_err, Result};
use clap::{clap_app, crate_version};
use std::path::Path;

#[tokio::main]
pub async fn run() -> Result<()> {
    let _ = simple_logger::SimpleLogger::new()
        .with_level(log::LevelFilter::Info)
        .init();
    let args = clap_app!(Duma =>
    (version: crate_version!())
    (author: "Matt Gathu <mattgathu@gmail.com>")
    (about: "A minimal file downloader")
    (@arg quiet: -q --quiet "quiet (no output)")
    (@arg continue: -c --continue "resume getting a partially-downloaded file")
    (@arg singlethread: -s --singlethread "download using only a single thread")
    (@arg headers: -H --headers "prints the headers sent by the HTTP server")
    (@arg FILE: -O --output +takes_value "write documents to FILE")
    (@arg AGENT: -U --useragent +takes_value "identify as AGENT instead of Duma/VERSION")
    (@arg SECONDS: -T --timeout +takes_value "set all timeout values to SECONDS")
    (@arg NUM_CONNECTIONS: -n --num_connections +takes_value "maximum number of concurrent connections (default is 8)")
    (@arg URL: +required +takes_value "url to download")
    )
    .get_matches_safe().unwrap_or_else(|e| e.exit());

    let url = utils::parse_url(
        args.value_of("URL")
            .ok_or_else(|| format_err!("missing URL argument"))?,
    )?;
    let quiet_mode = args.is_present("quiet");
    let file_name = args.value_of("FILE");
    let version = crate_version!();
    let resume_download = args.is_present("continue");
    let concurrent_download = !args.is_present("singlethread");
    let user_agent = args
        .value_of("AGENT")
        .unwrap_or(&format!("Duma/{}", version))
        .to_owned();
    let timeout = if let Some(secs) = args.value_of("SECONDS") {
        secs.parse::<u64>()?
    } else {
        30u64
    };
    let num_workers = if let Some(num) = args.value_of("NUM_CONNECTIONS") {
        num.parse::<usize>()?
    } else {
        2usize
    };
    let headers = request_headers_from_server(&url, timeout, &user_agent).await?;
    let fname = gen_filename(&url, args.value_of("FILE"), Some(&headers));

    // early exit if headers flag is present
    if args.is_present("headers") {
        print_headers(headers);
        return Ok(());
    }
    let ct_len = if let Some(val) = headers.get("Content-Length") {
        val.to_str()?.parse::<u64>().unwrap_or(0)
    } else {
        0u64
    };

    let headers = prep_headers(&fname, resume_download, &user_agent)?;
    let state_file_exists = Path::new(&format!("{}.st", fname)).exists();
    let chunk_size = 1_024_000_u64;
    let chunk_offsets =
        if state_file_exists && resume_download && concurrent_download && ct_len != 0 {
            Some(get_resume_chunk_offsets(&fname, ct_len, chunk_size)?)
        } else {
            None
        };

    log::info!("chunk_offsets {:?}", chunk_offsets);
    let bytes_on_disk = if resume_download {
        calc_bytes_on_disk(&fname)?
    } else {
        None
    };

    let conf = Config {
        user_agent,
        resume: resume_download,
        headers,
        file: fname.clone(),
        timeout,
        concurrent: concurrent_download,
        max_retries: 100,
        num_workers,
        bytes_on_disk,
        chunk_offsets,
        chunk_size,
        quiet_mode: args.is_present("quiet"),
    };

    match url.scheme() {
        #[cfg(feature = "enable-ftp")]
        "ftp" => ftp_download(url, quiet_mode, file_name),
        "http" | "https" => http_download(url, conf).await,
        _ => utils::gen_error(format!("unsupported url scheme '{}'", url.scheme())),
    }
}
