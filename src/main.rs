#!/usr/bin/env nix-shell
//! ```cargo
//! [dependencies]
//! thirtyfour = { version = "^0.32.0-rc.8", default-features = false, features = ["rustls-tls"] }
//! tokio = { version = "1", features = ["full"] }
//! serde = { version = "1", features = ["derive"] }
//! serde_json = "1"
//! color-eyre = "0.6.2"
//! clap = { version = "4", features = ["derive"] }
//! ```
/*
#! nix-shell -i rust-script -p rustc -p rust-script -p cargo -p yt-dlp
*/

#![warn(clippy::pedantic, clippy::nursery, clippy::style)]
#![deny(unused_must_use)]
use clap::{Parser, Subcommand};
use color_eyre::{
    eyre::{bail, eyre, OptionExt, WrapErr},
    owo_colors::OwoColorize,
    Result,
};
use std::{collections::HashMap, path::PathBuf, process::Output, sync::Arc, time::Duration};
use thirtyfour::prelude::*;
use tokio::{
    fs,
    fs::File,
    io::AsyncWriteExt,
    process::Command,
    sync::{Mutex, Semaphore},
    task::JoinSet,
    time::sleep,
};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Subcommands,
}

#[derive(Subcommand, Clone)]
enum Subcommands {
    Grab {
        grab: String,
        links_path: PathBuf,
        #[arg(long)]
        seasons: Option<u8>,
    },
    Download {
        links_path: PathBuf,
        output_dir: PathBuf,
        #[arg(long)]
        completed: Option<PathBuf>,
        #[arg(long)]
        slowdown: Option<u64>,
        #[arg(long)]
        threads: Option<usize>,
        #[arg(long = "yt-dlp")]
        yt_dlp_path: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let cli_args = Cli::parse();

    match cli_args.command {
        Subcommands::Grab {
            grab,
            links_path,
            seasons,
        } => {
            let links = grab_links(grab, seasons).await?;
            let str = serde_json::to_string_pretty(&links)?;
            let mut file = File::create(links_path).await?;
            file.write_all(str.as_bytes()).await?;
        }
        Subcommands::Download {
            links_path,
            output_dir,
            completed,
            threads,
            slowdown,
            yt_dlp_path,
        } => {
            let yt_dlp_path = yt_dlp_path.unwrap_or_else(|| PathBuf::from("yt-dlp"));
            match Command::new(&yt_dlp_path).output().await {
                Ok(_) => println!("yt-dlp found on path"),
                Err(e) => bail!("yt-dlp not found on path: {e:?}"),
            }
            let threads = threads.unwrap_or(2);
            if threads == 0 {
                bail!("no threads to download");
            }
            download(
                links_path,
                output_dir,
                threads,
                slowdown.unwrap_or(30),
                completed,
                yt_dlp_path,
            )
            .await?;
        }
    }
    Ok(())
}

const D20_SEASONS: u8 = 24;
const GC_SEASONS: u8 = 6;

async fn download(
    links_file: PathBuf,
    download_path: PathBuf,
    threads: usize,
    secs_slowdown: u64,
    completed: Option<PathBuf>,
    yt_dlp_path: PathBuf,
) -> Result<()> {
    let links = {
        let path = links_file;
        dbg!(&path.canonicalize());
        let links_str = fs::read_to_string(path).await?;
        serde_json::from_str(links_str.trim_matches(char::from(0)))?
    };
    download_all_links(
        links,
        download_path,
        threads,
        secs_slowdown,
        completed,
        yt_dlp_path,
    )
    .await
    .wrap_err("could not download")?;
    Ok(())
}

async fn download_all_links(
    links: HashMap<String, Vec<String>>,
    download_path: PathBuf,
    threads: usize,
    secs: u64,
    completed_path: Option<PathBuf>,
    yt_dlp_path: PathBuf,
) -> Result<()> {
    let yt_dlp_path = Arc::new(yt_dlp_path);

    if !download_path.is_dir() {
        fs::create_dir_all(&download_path).await?;
        let _ = dbg!(PathBuf::from(&download_path).canonicalize());
    }
    let download_path = Arc::new(download_path);

    let mut completed = if let Some(completed_path) = &completed_path {
        dbg!(&completed_path);
        let completed_links = if completed_path.is_file() {
            let content = fs::read_to_string(completed_path).await?;
            dbg!(&content);
            fs::remove_file(&completed_path).await?;
            dbg!(serde_json::from_str(content.trim_matches(char::from(0))).ok())
        } else {
            if let Some(parent) = completed_path.parent() {
                dbg!(&parent);
                fs::create_dir_all(parent).await?;
            }
            None
        };
        let completed_links = completed_links.unwrap_or_else(Vec::new);
        let mut file = File::create(&completed_path).await?;
        if completed_path.is_file() {
            file.write_all(serde_json::to_string(&completed_links)?.as_bytes())
                .await?;
        }
        Some((file, completed_links))
    } else {
        None
    };

    let links = if let Some((_, completed_links)) = completed.as_mut() {
        links
            .into_iter()
            .map(|(s, l)| {
                (
                    s,
                    l.into_iter()
                        .filter(|x| !completed_links.contains(x))
                        .collect::<Vec<_>>(),
                )
            })
            .collect()
    } else {
        links
    };

    let semaphore = Arc::new(Semaphore::new(threads));
    let mut tasks_set = JoinSet::new();
    for (season_num, episodes_in_season) in links {
        let season_download_path = download_path.join(season_num.to_string());
        fs::create_dir_all(&season_download_path).await?;
        let season_download_path = Arc::new(season_download_path);
        for link in episodes_in_season {
            if !link.contains("dropout.tv") {
                continue;
            }
            tasks_set.spawn({
                let season_download_path = season_download_path.clone();
                let semaphore = semaphore.clone();
                let yt_dlp_path = yt_dlp_path.clone();
                async move {
                    let permit = semaphore.acquire_owned().await?;
                    let result = download_link(&link, season_download_path, yt_dlp_path).await?;
                    println!("done running, slowing...");
                    sleep(Duration::from_secs(secs)).await;
                    drop(permit);
                    Ok::<_, color_eyre::Report>((result, link))
                }
            });
        }
    }

    let completed = Arc::new(Mutex::new(match (completed, completed_path) {
        (Some((f, v)), Some(p)) => Some((f, p, v)),
        (None, None) => None,
        s => bail!("invalid state: {s:#?}"),
    }));

    while let Some(result) = tasks_set.join_next().await {
        tokio::spawn(handle_set(result.map_err(Into::into), completed.clone()));
    }
    Ok(())
}

#[allow(clippy::type_complexity)]
async fn handle_set(
    result: Result<Result<(Output, String)>>,
    completed: Arc<Mutex<Option<(File, PathBuf, Vec<String>)>>>,
) -> Result<()> {
    let mut stdout = tokio::io::stdout();
    let (output, link) = result??;
    if output.status.success() {
        {
            let mut lock = completed.lock().await;
            if let Some((file, path, completed_links)) = lock.as_mut() {
                *file = File::create(path).await?;
                let str = serde_json::to_string(completed_links)?;
                if !completed_links.contains(&link) {
                    completed_links.push(link.clone());
                };
                file.write_all(str.as_bytes()).await?;
            }
        }
        stdout
            .write_all(
                format!("success! \"{link}\"\n")
                    .green()
                    .to_string()
                    .as_bytes(),
            )
            .await?;
    } else {
        // failure
        stdout
            .write_all(
                format!("failure for link \"{link}\" ! \n")
                    .red()
                    .to_string()
                    .as_bytes(),
            )
            .await?;
        stdout.write_all(&output.stderr).await?;
    }
    stdout.write_all(b"\n").await?;
    stdout.flush().await?;
    Ok(())
}

async fn download_link(link: &str, path: Arc<PathBuf>, cmd_path: Arc<PathBuf>) -> Result<Output> {
    println!("{}", format!("running \"{link}\"").bright_yellow().bold());
    let path = path.to_string_lossy();
    Command::new("/usr/bin/env")
        .arg("bash")
        .arg("-c")
        .args(&[format!(
            "{} --referer 'https://www.dropout.tv/' --netrc -P {path} --write-subs {link}",
            cmd_path.to_string_lossy()
        )])
        .output()
        .await
        .map_err(Into::into)
}

const DROPOUT_URL: &str = "https://www.dropout.tv";
#[inline]
fn dropout(string: &str) -> String {
    format!("{DROPOUT_URL}{string}")
}

async fn grab_links(grab: String, seasons: Option<u8>) -> Result<HashMap<String, Vec<String>>> {
    let driver = WebDriver::new("http://localhost:4444", DesiredCapabilities::firefox()).await?;
    let links_res = grab_links_grab(&driver, grab, seasons).await;
    driver.quit().await?;
    links_res
}

async fn log_in(driver: &WebDriver) -> Result<()> {
    driver.goto(dropout("/login")).await?;
    sleep(Duration::from_secs(10)).await;
    let accept_cookies = driver.find(By::Css("button[data-nav='eyJleHBlcmllbmNlIjoia2V0Y2gtY29uc2VudC1iYW5uZXIiLCJuYXYtaW5kZXgiOjJ9']")).await?;
    accept_cookies.wait_until().displayed().await?;
    accept_cookies.wait_until().enabled().await?;
    accept_cookies.wait_until().clickable().await?;
    accept_cookies.click().await?;
    let email_enter = driver.find(By::Id("signin-email-input")).await?;
    email_enter.send_keys(EMAIL).await?;
    let email_click = driver.find(By::Id("signin-email-submit")).await?;
    email_click.click().await?;
    let password_box = driver.find(By::Id("signin-password-input")).await?;
    password_box.send_keys(PASSWORD).await?;
    let password_click = driver.find(By::Id("signin-password-submit")).await?;
    password_click.click().await?;
    println!("logged in");
    Ok(())
}

async fn grab_links_grab(
    driver: &WebDriver,
    next_arg: String,
    seasons: Option<u8>,
) -> Result<HashMap<String, Vec<String>>> {
    log_in(driver).await?;
    let (count, url_prefix) = match next_arg.as_str() {
        "d20" => (D20_SEASONS, "dimension-20"),
        "gc" => (GC_SEASONS, "game-changer"),
        url => seasons
            .map(|s| (s, url))
            .ok_or_eyre("no seasons flag with arbitrary URL")?,
    };

    let mut links = HashMap::new();

    for i in 1..=count {
        links.insert(
            i.to_string(),
            get_links_season(
                dbg!(dropout(format!("/{url_prefix}/season:{i}").as_str())).as_str(),
                driver,
            )
            .await?,
        );
    }

    Ok(links)
}

async fn get_links_season(season_url: &str, driver: &WebDriver) -> Result<Vec<String>> {
    let mut links = vec![];
    driver.goto(season_url).await?;
    sleep(Duration::from_secs(20)).await;
    let episodes = driver.find_all(By::ClassName("browse-item-link")).await?;
    if episodes.is_empty() {
        bail!("invalid number of episodes");
    }
    for (index, episode) in episodes.into_iter().enumerate() {
        if episode.tag_name().await? != "a" {
            bail!("invalid tag! {index} {:?}", &episode)
        }
        let link = episode
            .attr("href")
            .await?
            .ok_or_else(|| eyre!("no link value"))?;
        links.push(dbg!(link));
    }
    Ok(links)
}

const PASSWORD: &str = include!("../auth.in").1;
const EMAIL: &str = include!("../auth.in").0;
