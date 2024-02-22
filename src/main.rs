#!/usr/bin/env nix-shell
//! ```cargo
//! [dependencies]
//! thirtyfour = { version = "0.32.0-rc.8", default-features = false, features = ["rustls-tls"] }
//! tokio = { version = "1", features = ["full"] }
//! reqwest = { version = "0.11.18", default-features = false, features = ["json", "rustls"] }
//! serde = { version = "1", features = ["derive"] }
//! serde_json = "1.0.104"
//! color-eyre = "0.6.2"
//! ```
/*
#! nix-shell -i rust-script -p rustc -p rust-script -p cargo -p yt-dlp
*/

#[warn(clippy::pedantic, clippy::nursery, clippy::style)]
#[deny(unused_must_use)]

use std::{process::Output, sync::Arc};
use color_eyre::{eyre::{bail, WrapErr, eyre}, Result};
use tokio::{io::AsyncWriteExt, process::{Child, Command}, task::JoinSet, sync::Semaphore};
use thirtyfour::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    run().await
}

const D20_SEASONS: u8 = 21;
const GC_SEASONS: u8 = 6;

async fn run() -> Result<()> {
    let mut args = std::env::args();
    args.next();
    if let Some(arg) = args.next() {
        match arg.as_str() {
            "grab" => {
                let links = grab_links().await?;
                let links_struct = Links { links };
                let str = serde_json::to_string(&links_struct)?;
                let mut file = tokio::fs::File::create("links.json").await?;
                file.write_all(str.as_bytes()).await?;
                Ok(())
            }
            "download" => download().await,
            s => Err(eyre!("{s:?} neither??")),
        }
    } else {
        Err(eyre!("no arg!"))
    }
}

async fn download() -> Result<()> {
    // let mut geckodriver_command = start_geckodriver().await?;
    let links = {
        let links_str = tokio::fs::read_to_string("links.json").await?;
        let Links { links } = serde_json::from_str(&links_str)?;
        links
    };
    download_all_links(links).await.wrap_err("could not download")?;
    // geckodriver_command.kill().await?;
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Links {
    links: Vec<String>,
}

async fn download_all_links(links: Vec<String>) -> Result<()> {
    let semaphore = Arc::new(Semaphore::new(4));
    let mut tasks_set = JoinSet::new();
    for link in links {
        let semaphore = semaphore.clone();
        let permit = semaphore.acquire_owned().await?;
        tasks_set.spawn(async move {
            let result = download_link(&link).await?;
            drop(permit);
            Ok::<_, color_eyre::Report>((result, link))
        });
    }
    let mut stdout = tokio::io::stdout();
    while let Some(result) = tasks_set.join_next().await {
        let (output, link) = result??;
        if output.status.success() {
            println!("success! {link}");
            continue;
        }

        // failure
        stdout.write_all(format!("failure for link \"{}\" ! \n", link).as_bytes()).await?;
        stdout.write_all(&output.stderr).await?;
    }
    Ok(())
}

async fn download_link(link: &str) -> Result<Output> {
    Ok(
        Command::new("/usr/bin/env")
            .arg("bash")
            .arg("-c")
            .args(&[format!("yt-dlp --referer 'https://www.dropout.tv/' --netrc --format 'best' {} -P binaries", link)])
            .output()
            .await?,
    )
}

async fn start_geckodriver() -> Result<Child> {
    Command::new("/home/aditya/.nix-profile/bin/killall").output().await.wrap_err("cannot killall")?;
    let child = Command::new("/home/aditya/.nix-profile/bin/geckodriver")
        .spawn()?;
    Ok(child)
}

#[inline]
fn dropout(string: &str) -> String {
    const PREPEND: &str = "https://www.dropout.tv";
    format!("{}{}", PREPEND, string)
}

async fn grab_links() -> Result<Vec<String>> {
    let driver = WebDriver::new("http://localhost:4444", DesiredCapabilities::firefox()).await?;
    let links_res = grab_links_grab(&driver).await;
    driver.quit().await?;
    links_res
}

async fn grab_links_grab(driver: &WebDriver) -> Result<Vec<String>> {
    {
        driver.goto(dropout("/login")).await?;
        let email_enter = driver.find(By::Id("signin-email-input")).await?;
        email_enter.send_keys(EMAIL).await?;
        let email_click = driver.find(By::Id("signin-email-submit")).await?;
        email_click.click().await?;
        let password_box = driver.find(By::Id("signin-password-input")).await?;
        password_box.send_keys(PASSWORD).await?;
        let password_click = driver.find(By::Id("signin-password-submit")).await?;
        password_click.click().await?;
    }
    println!("logged in");

    let mut links = vec![];
    // for i in 1..=D20_SEASONS {
    for i in 1..=GC_SEASONS {
        let mut dimension_twenty_links =
            get_links_season(
                (
                    // dropout("/dimension-20/season:") + i.to_string().as_str()
                    dropout("/game-changer/season:") + i.to_string().as_str()
                ).as_str(),
                &driver,
            ).await?;
        links.append(&mut dimension_twenty_links)
    }

    println!("logged in");

    Ok(links)
}

async fn get_links_season(season_url: &str, driver: &WebDriver) -> Result<Vec<String>> {
    let mut links = vec![];
    driver.goto(season_url).await?;
    let episodes = driver.find_all(By::ClassName("browse-item-link")).await?;
    if episodes.len() == 0 {
        bail!("invalid number of episodes");
    }
    for (index, episode) in episodes.into_iter().enumerate() {
        if episode.tag_name().await? != "a" {
            bail!("invalid tag! {index} {:?}", &episode)
        }
        let link = episode.attr("href").await?.ok_or_else(|| eyre!("no link value"))?;
        links.push(dbg!(link));
    }
    Ok(links)
}

/*
// for animepahe only

#[inline]
fn animepahe(str: &str) -> String {
    String::from("https://animepahe.ru") + str
}

async fn grab_links() -> Result<()> {
    let driver = WebDriver::new("http://localhost:4444", DesiredCapabilities::firefox()).await.wrap_err("could not start ")?;
    let tools = FirefoxTools::new(driver.handle.clone());
    tools.install_addon("/home/aditya/Downloads/ublock.signed.xpi", None).await?;
    driver.goto(dbg!(animepahe("/anime/f6b763ce-aaf7-50c8-2bd0-a7e5dd9d4445"))).await?;

    tokio::time::sleep(std::time::Duration::new(5, 0)).await;

    let links = driver.find_all(By::ClassName("play")).await?;

    println!("total {}", links.len());

    for (index, link) in links.into_iter().enumerate() {
        println!("element {index}");
        let link = link.attr("href").await?.ok_or_else(|| eyre!("no href attr"))?;
        println!("{link}");
        driver.goto(link).await?;
        println!("waiting...");
        tokio::time::sleep(std::time::Duration::new(5, 0)).await;
        println!("done");
        driver.find(By::Id("downloadMenu")).await?.click().await?;
        let dropdown_items = driver.find_all(By::ClassName("dropdown-item")).await?;
        for item in dropdown_items.into_iter().rev() {

            // dbg!(item);
        }
    }

    println!("done!!!");

    tokio::time::sleep(std::time::Duration::new(300, 0)).await;

    driver.quit().await?;
    Ok(())
}
 */


const PASSWORD: &str = "Aditya99*3";
const EMAIL: &str = "adityakomp@gmail.com";
