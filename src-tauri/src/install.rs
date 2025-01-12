use std::{io::ErrorKind, path::PathBuf, sync::atomic};

use async_compat::CompatExt;
use async_compression::futures::bufread::ZstdDecoder;
use futures::{StreamExt, TryStreamExt};
use semver::Version;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Url};
use tauri_plugin_http::reqwest::{self, IntoUrl, Response};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncWriteExt},
};
use tokio_util::bytes::BytesMut;

use crate::{wine_util::get_wine_path, PatchManifest};

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

#[derive(thiserror::Error, Debug)]
pub(crate) enum InstallError {
    #[error("missing root URL")]
    MissingRootUrl,
    #[error("unknown version")]
    UnknownVersion,
    #[error("unsupported architecture")]
    UnsupportedArch,
    #[error("unsupported operating system")]
    UnsupportedOS,
    #[error("failed to parse URL: {0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("net error: {0}")]
    Reqwest(#[from] tauri_plugin_http::reqwest::Error),
    #[error("version error: {0}")]
    InvalidVersion(#[from] semver::Error),
    #[error("failed to create directory: {0}")]
    FailedCreateDir(std::io::Error),
    #[error("Tauri error: {0}")]
    Tauri(#[from] tauri::Error),
    #[error("invalid archive path: {0}")]
    InvalidArchivePath(PathBuf),
}

pub(crate) async fn do_install(
    app: &AppHandle,
    http: reqwest::Client,
    install_dir: PathBuf,
) -> Result<(), InstallError> {
    let mut progress = InstallProgress::default();

    let updater_endpoints = app
        .config()
        .plugins
        .0
        .get("updater")
        .and_then(|o| o.get("endpoints").and_then(|o| o.as_array()));

    let first_endpoint =
        updater_endpoints.and_then(|vec| vec.get(0).and_then(|endpoint| endpoint.as_str()));

    let mut root_url = first_endpoint
        .map(|input| Url::parse(input))
        .transpose()?
        .ok_or(InstallError::MissingRootUrl)?;
    root_url.set_path("assets/PackWisely/");

    progress.emit_msg(app, "Fetching channels")?;
    let channels_url = root_url.join("channels.json")?;
    let channels_json: Vec<ChannelManifest> = progress.get_json(&http, channels_url).await?;

    let channel_name = &channels_json[0].name;
    let channel_root = root_url.join(&(channel_name.to_string() + "/"))?;

    progress.emit_msg(app, "Fetching versions")?;
    let versions_url = channel_root.join("versions.json")?;
    let versions_json: Vec<VersionManifest> = progress.get_json(&http, versions_url).await?;

    let version = Version::parse("0.1.0-alpha.1")?;
    let version_root = channel_root.join(&(version.to_string() + "/"))?;

    let version_mf = versions_json
        .iter()
        .find(|mf| mf.version == version)
        .ok_or(InstallError::UnknownVersion)?;

    let mut os_ok_list: Vec<_> = version_mf
        .platforms
        .iter()
        .filter(|mf| mf.os == std::env::consts::OS)
        .collect();

    let wine_path = get_wine_path().ok();
    if wine_path.is_some() {
        // Append Wine-compatible entries after exact matches.
        os_ok_list.extend(version_mf.platforms.iter().filter(|mf| mf.os == "windows"));
    }
    if os_ok_list.is_empty() {
        return Err(InstallError::UnsupportedOS.into());
    }

    let arch_ok_list: Vec<_> = os_ok_list
        .iter()
        .filter(|mf| mf.arch == std::env::consts::ARCH)
        .collect();
    if arch_ok_list.is_empty() {
        return Err(InstallError::UnsupportedArch.into());
    }

    let platform_mf = os_ok_list[0];
    let platform_root =
        version_root.join(&(platform_mf.os.clone() + "/" + &platform_mf.arch.clone() + "/"))?;

    progress.emit_msg(app, "Fetching platform manifest")?;
    let manifest_url = platform_root.join("manifest.json")?;
    let manifest_json: PatchManifest = progress.get_json(&http, manifest_url).await?;

    match tokio::fs::read_dir(&install_dir).await {
        Ok(read_install_dir) => {
            progress.emit_msg(app, "Verifying files")?;

            todo!()
        }
        Err(e) => {
            if e.kind() != ErrorKind::NotFound {
                return Err(e.into());
            }

            tokio::fs::create_dir_all(&install_dir)
                .await
                .map_err(|e| InstallError::FailedCreateDir(e))?;
        }
    }

    progress.disk.max = manifest_json
        .new_files
        .iter()
        .chain(manifest_json.diff_files.iter())
        .map(|file| file.len)
        .sum();

    progress.disk.known = true;

    if !manifest_json.diff_files.is_empty() {
        progress.emit_msg(app, "Updating existing files")?;
        let diff_tar_url = platform_root.join("diff.tar.zst")?;

        todo!()
    }

    if !manifest_json.new_files.is_empty() {
        progress.emit_msg(app, "Downloading new files")?;
        let raw_tar_url = platform_root.join("raw.tar.zst")?;
        let raw_tar_response = http.get(raw_tar_url).send().await?;

        progress.net.max = raw_tar_response.content_length().unwrap_or(0);
        progress.net.known = true;
        progress.emit(app)?;

        // Use atomic counter for Send-safety.
        let response_net_counter = atomic::AtomicU64::new(0);
        let response_stream = raw_tar_response
            .bytes_stream()
            .map(|chunk| match chunk {
                Ok(bytes) => {
                    response_net_counter.fetch_add(bytes.len() as u64, atomic::Ordering::Relaxed);
                    Ok(bytes)
                }
                Err(error) => Err(std::io::Error::new(ErrorKind::Other, error)),
            })
            .into_async_read();
        let tar_stream = ZstdDecoder::new(response_stream);

        let archive = async_tar::Archive::new(tar_stream);
        let mut entries = archive.entries()?;
        let mut read_buf = BytesMut::with_capacity(1024 * 32);
        while let Some(entry) = entries.next().await.transpose()? {
            let relative_path = entry.path()?.into_owned();
            let dst_path = install_dir.join(relative_path);

            tokio::fs::create_dir_all(
                dst_path
                    .parent()
                    .ok_or_else(|| InstallError::InvalidArchivePath(dst_path.clone()))?,
            )
            .await?;
            let mut dst_file = File::create(dst_path).await?;

            let mut entry_reader = entry.compat();
            while entry_reader.read_buf(&mut read_buf).await? != 0 {
                let written = dst_file.write_buf(&mut read_buf.split()).await?;

                progress.net.value += response_net_counter.swap(0, atomic::Ordering::Relaxed);
                progress.disk.value += written as u64;
                progress.emit(app)?;
            }
        }
        progress.net.value += response_net_counter.swap(0, atomic::Ordering::Relaxed);
    }

    if !manifest_json.stale_files.is_empty() {
        progress.emit_msg(app, "Removing old files")?;
        for file in manifest_json.stale_files {
            tokio::fs::remove_file(&install_dir.join(file)).await?;
        }
    }

    Ok(())
}

#[derive(Debug, Default, Clone, Serialize)]
struct InstallProgress {
    net: ProgressState,
    disk: ProgressState,
    message: String,
}

impl InstallProgress {
    fn emit(&self, app: &AppHandle) -> Result<(), tauri::Error> {
        app.emit("install-progress", self)
    }

    fn emit_msg(&mut self, app: &AppHandle, message: &str) -> Result<(), tauri::Error> {
        self.message = message.into();
        self.emit(app)
    }

    async fn get_and_send(
        &mut self,
        http: &reqwest::Client,
        url: impl IntoUrl,
    ) -> tauri_plugin_http::reqwest::Result<Response> {
        let response = http.get(url).send().await?;
        self.net.add_both(response.content_length().unwrap_or(0));
        Ok(response)
    }

    async fn get_json<T: DeserializeOwned>(
        &mut self,
        http: &reqwest::Client,
        url: impl IntoUrl,
    ) -> tauri_plugin_http::reqwest::Result<T> {
        Ok(self.get_and_send(http, url).await?.json().await?)
    }
}

#[derive(Debug, Default, Clone, Serialize)]
struct ProgressState {
    value: u64,
    max: u64,
    known: bool,
}

impl ProgressState {
    fn add(&mut self, value: u64, target: u64) {
        self.value += value;
        self.max += target;
    }

    fn add_both(&mut self, value: u64) {
        self.add(value, value);
    }
}
