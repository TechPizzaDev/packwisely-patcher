use std::path::PathBuf;

use semver::Version;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Url};
use tauri_plugin_http::reqwest;
use tokio::fs::{File, OpenOptions};

#[derive(Debug, Clone, Deserialize)]
struct ChannelManifest {
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct VersionManifest {
    version: Version,
    platforms: Vec<PlatformManifest>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlatformManifest {
    os: String,
    arch: String,
    exe_path: String,
}

pub(crate) async fn do_install(
    app: &AppHandle,
    http_client: reqwest::Client,
    install_dir: PathBuf,
) -> anyhow::Result<()> {
    let root_url = Url::parse("https://techpizza-web.nickac.dev/assets/")?;
    let game_root_url = root_url.join("PackWisely/")?;

    let channels_url = game_root_url.join("channels.json")?;
    let channels_json: Vec<ChannelManifest> =
        http_client.get(channels_url).send().await?.json().await?;
    let channel_name = &channels_json[0].name;
    let channel_url = game_root_url.join(&(channel_name.to_string() + "/"))?;

    let versions_url = channel_url.join("versions.json")?;
    let versions_json: Vec<VersionManifest> =
        http_client.get(versions_url).send().await?.json().await?;

    app.emit("install-finished", format!("{:?}", versions_json))
        .unwrap();

    Ok(())
}
