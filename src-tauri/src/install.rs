use std::{
    collections::HashMap,
    io::{ErrorKind, Read, Seek, Write},
    path::PathBuf,
    sync::atomic,
    time::Instant,
};

use async_compat::CompatExt;
use async_compression::tokio::bufread::ZstdDecoder;
use fast_rsync::sum_hash::{Blake3Hash, SumHash};
use futures::StreamExt;
use memmap2::Mmap;
use semver::Version;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Url};
use tauri_plugin_http::reqwest::{self, IntoUrl, Response};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
};
use tokio_util::io::StreamReader;

use crate::{
    file_util::{copy_dir, CopyError},
    wine_util::get_wine_path,
    PatchManifest,
};

#[derive(Debug, Clone, Deserialize)]
struct ChannelManifest {
    name: String,
}
impl ChannelManifest {
    fn join_url(&self, root_url: &Url) -> Result<Url, url::ParseError> {
        root_url.join(&(self.name.to_string() + "/"))
    }
}

#[derive(Debug, Clone, Deserialize)]
struct VersionManifest {
    version: Version,
    platforms: Vec<PlatformManifest>,
}
impl VersionManifest {
    fn join_url(&self, channel_url: &Url) -> Result<Url, url::ParseError> {
        channel_url.join(&(self.version.to_string() + "/"))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlatformManifest {
    os: String,
    arch: String,
    exe_path: String,
}
impl PlatformManifest {
    fn join_url(&self, version_url: &Url) -> Result<Url, url::ParseError> {
        version_url.join(&(self.os.clone() + "/" + &self.arch.clone() + "/"))
    }
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum InstallError {
    #[error("missing root URL")]
    MissingRootUrl,
    #[error("unknown release channel")]
    UnknownChannel,
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
    CreateDir(std::io::Error),
    #[error("Tauri error: {0}")]
    Tauri(#[from] tauri::Error),
    #[error("invalid archive path: {0}")]
    InvalidArchivePath(PathBuf),
    #[error("invalid patch in installed directory: {0}")]
    InvalidInstalledPatch(serde_json::Error),
    #[error("missing previous version")]
    MissingPreviousVersion,
    #[error("unexpected file in archive: {0}")]
    UnexpectedArchiveFile(PathBuf),
    #[error("failed to apply diff: {0}")]
    DiffApplyError(#[from] fast_rsync::ApplyError),
    #[error("wrong size: {expected} != {actual}")]
    WrongSize { expected: u64, actual: u64 },
    #[error("wrong hash: 0x{expected} != 0x{actual}")]
    WrongHash { expected: String, actual: String },
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("failed to copy files: {0}")]
    CopyError(#[from] CopyError),
}

fn get_root_url(app: &AppHandle) -> Result<Url, InstallError> {
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
    Ok(root_url)
}

fn join_install_dir(
    channel_dir: &PathBuf,
    version: &Version,
    platform_mf: &PlatformManifest,
) -> PathBuf {
    channel_dir.join(format!(
        "{}/{}-{}",
        version, platform_mf.os, platform_mf.arch
    ))
}

pub(crate) async fn do_install(
    app: &AppHandle,
    http: &reqwest::Client,
    install_dir: PathBuf,
) -> Result<(), InstallError> {
    let mut progress = InstallProgress::default();

    let root_url = get_root_url(app)?;

    let channels = get_channels(app, http, &mut progress, &root_url).await?;
    let channel_mf = channels.get(0).ok_or(InstallError::UnknownChannel)?;
    let channel_url = channel_mf.join_url(&root_url)?;

    let channel_dir = install_dir.join(channel_mf.name.to_string() + "/");
    let old_patch_mf = verify_channel_dir(app, &mut progress, &channel_dir).await?;

    let versions = get_versions(app, http, &mut progress, &root_url, channel_mf).await?;
    let version_mf = versions.last().ok_or(InstallError::UnknownVersion)?;
    if let Some(mf) = &old_patch_mf {
        if mf.version == version_mf.version {
            return Ok(());
        }
    }
    let version_url = version_mf.join_url(&channel_url)?;

    let platforms = get_platforms(&version_mf)?;
    let platform_mf = &platforms[0];
    let platform_url = platform_mf.join_url(&version_url)?;

    let old_install_dir =
        old_patch_mf.map(|mf| join_install_dir(&channel_dir, &mf.version, platform_mf));

    let new_install_dir = join_install_dir(&channel_dir, &version_mf.version, platform_mf);
    tokio::fs::create_dir_all(&new_install_dir)
        .await
        .map_err(|e| InstallError::CreateDir(e))?;

    let new_patch_mf = get_patch(app, http, &mut progress, &platform_url).await?;
    install_patch(
        app,
        http,
        &mut progress,
        &platform_url,
        old_install_dir,
        new_install_dir,
        new_patch_mf.clone(),
    )
    .await?;

    let mut patch_mf_file = File::create(channel_dir.join("manifest.json")).await?;
    patch_mf_file
        .write_all(&serde_json::to_vec(&new_patch_mf)?)
        .await?;

    Ok(())
}

async fn get_channels(
    app: &AppHandle,
    http: &reqwest::Client,
    progress: &mut InstallProgress,
    root_url: &Url,
) -> Result<Vec<ChannelManifest>, InstallError> {
    progress.emit_msg(app, "Fetching channels")?;
    let channels_url = root_url.join("channels.json")?;
    let channels_json = progress.get_json(http, channels_url).await?;
    Ok(channels_json)
}

async fn get_versions(
    app: &AppHandle,
    http: &reqwest::Client,
    progress: &mut InstallProgress,
    root_url: &Url,
    channel_mf: &ChannelManifest,
) -> Result<Vec<VersionManifest>, InstallError> {
    progress.emit_msg(app, "Fetching versions")?;
    let versions_url = channel_mf.join_url(root_url)?.join("versions.json")?;
    let versions_json = progress.get_json(http, versions_url).await?;
    Ok(versions_json)
}

fn get_platforms(version_mf: &VersionManifest) -> Result<Vec<PlatformManifest>, InstallError> {
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
        .into_iter()
        .filter(|mf| mf.arch == std::env::consts::ARCH)
        .cloned()
        .collect();
    if arch_ok_list.is_empty() {
        return Err(InstallError::UnsupportedArch.into());
    }
    Ok(arch_ok_list)
}

async fn get_patch(
    app: &AppHandle,
    http: &reqwest::Client,
    progress: &mut InstallProgress,
    platform_url: &Url,
) -> Result<PatchManifest, InstallError> {
    progress.emit_msg(app, "Fetching platform manifest")?;
    let manifest_url = platform_url.join("manifest.json")?;
    let manifest_json = progress.get_json(&http, manifest_url).await?;
    Ok(manifest_json)
}

async fn verify_channel_dir(
    app: &AppHandle,
    progress: &mut InstallProgress,
    channel_dir: &PathBuf,
) -> Result<Option<PatchManifest>, InstallError> {
    progress.emit_msg(app, "Verifying install directory")?;

    match File::open(channel_dir.join("manifest.json")).await {
        Ok(mut file) => {
            let mut str = String::new();
            file.read_to_string(&mut str).await?;
            let patch_mf =
                serde_json::from_str(&str).map_err(|e| InstallError::InvalidInstalledPatch(e))?;
            Ok(Some(patch_mf))
        }
        Err(err) => {
            if err.kind() == ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(err.into())
            }
        }
    }
}

async fn install_patch(
    app: &AppHandle,
    http: &reqwest::Client,
    progress: &mut InstallProgress,
    platform_url: &Url,
    old_install_dir: Option<PathBuf>,
    new_install_dir: PathBuf,
    new_patch_mf: PatchManifest,
) -> Result<(), InstallError> {
    progress.disk.max = new_patch_mf
        .new_files
        .iter()
        .chain(new_patch_mf.diff_files.iter())
        .map(|file| file.len)
        .sum();

    progress.disk.known = true;

    let mut read_buf = Box::new([0u8; 1024 * 64]);
    let mut delta_buf = Vec::with_capacity(1024 * 64);

    let mut emit_timestamp = Instant::now();

    // Use atomic counter for Send-safety.
    let response_net_counter = atomic::AtomicU64::new(0);

    let mut files_to_remove = Vec::new();

    if !new_patch_mf.diff_files.is_empty() {
        progress.emit_msg(app, "Updating existing files")?;

        let old_install_dir = old_install_dir
            .as_ref()
            .ok_or(InstallError::MissingPreviousVersion)?;

        let mut diff_set = HashMap::with_capacity(new_patch_mf.diff_files.len());
        for file in new_patch_mf.diff_files.iter() {
            diff_set.insert(file.path.as_str(), (file.len, &file.hash));
        }

        let diff_tar_url = platform_url.join("diff.tar.zst")?;
        let diff_tar_response = http.get(diff_tar_url).send().await?;

        progress.net.max += diff_tar_response.content_length().unwrap_or(0);
        progress.net.known = true;
        progress.emit(app)?;

        let response_stream =
            StreamReader::new(diff_tar_response.bytes_stream().map(|chunk| match chunk {
                Ok(bytes) => {
                    response_net_counter.fetch_add(bytes.len() as u64, atomic::Ordering::Relaxed);
                    Ok(bytes)
                }
                Err(error) => Err(std::io::Error::new(ErrorKind::Other, error)),
            }));
        let tar_stream = ZstdDecoder::new(response_stream).compat();
        let archive = async_tar::Archive::new(tar_stream);
        let mut entries = archive.entries()?;

        while let Some(mut entry) = entries.next().await.transpose()? {
            let relative_path = entry.path()?.into_owned();
            let (dst_size, dst_hash) = *diff_set
                .get(&relative_path.to_string_lossy().into_owned().as_str())
                .ok_or(InstallError::UnexpectedArchiveFile((&relative_path).into()))?;

            let src_path = old_install_dir.join(&relative_path);
            let dst_path = new_install_dir.join(&relative_path);
            tokio::fs::create_dir_all(
                dst_path
                    .parent()
                    .ok_or_else(|| InstallError::InvalidArchivePath(dst_path.clone()))?,
            )
            .await
            .map_err(|e| InstallError::CreateDir(e))?;

            let mut dst_file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(dst_path)?;
            dst_file.set_len(dst_size)?;
            let mut dst_actual_hash = Blake3Hash::default();

            let src_file = File::open(&src_path).await?;
            let src_mmap = unsafe { Mmap::map(&src_file) }?;
            loop {
                let read = futures::AsyncReadExt::read(&mut entry, read_buf.as_mut()).await?;
                if read == 0 {
                    break;
                }
                delta_buf.extend_from_slice(&read_buf[..read]);

                let next_timestamp = Instant::now();
                if (next_timestamp - emit_timestamp).as_secs_f32() > 0.05 {
                    emit_timestamp = next_timestamp;

                    progress.net.value += response_net_counter.swap(0, atomic::Ordering::Relaxed);
                    progress.emit(app)?;
                }
            }

            fast_rsync::apply_limited(&src_mmap, &delta_buf, &mut dst_file, dst_size as usize)?;
            delta_buf.clear();
            dst_file.flush()?;

            let dst_actual_size = dst_file.stream_position()?;
            progress.disk.value += dst_actual_size;
            if dst_size != dst_actual_size {
                return Err(InstallError::WrongSize {
                    expected: dst_size,
                    actual: dst_actual_size,
                });
            }

            dst_file.seek(std::io::SeekFrom::Start(0))?;
            loop {
                let len = dst_file.read(read_buf.as_mut())?;
                if len == 0 {
                    break;
                }
                dst_actual_hash.update(&read_buf[..len]);
            }
            let dst_actual_hash = dst_actual_hash.finish();
            if dst_hash != &dst_actual_hash {
                return Err(InstallError::WrongHash {
                    expected: hex::encode(dst_hash),
                    actual: hex::encode(dst_actual_hash),
                });
            }
            files_to_remove.push(src_path);
        }
        progress.net.value += response_net_counter.swap(0, atomic::Ordering::Relaxed);
    }

    if !new_patch_mf.new_files.is_empty() {
        progress.emit_msg(app, "Downloading new files")?;

        let mut new_set = HashMap::with_capacity(new_patch_mf.new_files.len());
        for file in new_patch_mf.new_files.iter() {
            new_set.insert(file.path.as_str(), (file.len, &file.hash));
        }

        let raw_tar_url = platform_url.join("raw.tar.zst")?;
        let raw_tar_response = http.get(raw_tar_url).send().await?;

        progress.net.max += raw_tar_response.content_length().unwrap_or(0);
        progress.net.known = true;
        progress.emit(app)?;

        let response_stream =
            StreamReader::new(raw_tar_response.bytes_stream().map(|chunk| match chunk {
                Ok(bytes) => {
                    response_net_counter.fetch_add(bytes.len() as u64, atomic::Ordering::Relaxed);
                    Ok(bytes)
                }
                Err(error) => Err(std::io::Error::new(ErrorKind::Other, error)),
            }));
        let tar_stream = ZstdDecoder::new(response_stream).compat();
        let archive = async_tar::Archive::new(tar_stream);
        let mut entries = archive.entries()?;

        while let Some(mut entry) = entries.next().await.transpose()? {
            let relative_path = entry.path()?.into_owned();
            let (dst_size, dst_hash) = *new_set
                .get(&relative_path.to_string_lossy().into_owned().as_str())
                .ok_or(InstallError::UnexpectedArchiveFile((&relative_path).into()))?;

            let dst_path = new_install_dir.join(relative_path);
            tokio::fs::create_dir_all(
                dst_path
                    .parent()
                    .ok_or_else(|| InstallError::InvalidArchivePath(dst_path.clone()))?,
            )
            .await
            .map_err(|e| InstallError::CreateDir(e))?;

            let mut dst_file = File::create(dst_path).await?;
            dst_file.set_len(dst_size).await?;
            let mut dst_actual_hash = Blake3Hash::default();
            loop {
                let read = futures::AsyncReadExt::read(&mut entry, read_buf.as_mut()).await?;
                if read == 0 {
                    break;
                }
                let mut split = &read_buf[..read];
                dst_actual_hash.update(&split);

                let written = dst_file.write_buf(&mut split).await?;
                progress.disk.value += written as u64;

                let next_timestamp = Instant::now();
                if (next_timestamp - emit_timestamp).as_secs_f32() > 0.05 {
                    emit_timestamp = next_timestamp;

                    progress.net.value += response_net_counter.swap(0, atomic::Ordering::Relaxed);
                    progress.emit(app)?;
                }
            }
            dst_file.flush().await?;

            let dst_actual_size = dst_file.stream_position().await?;
            if dst_size != dst_actual_size {
                return Err(InstallError::WrongSize {
                    expected: dst_size,
                    actual: dst_actual_size,
                });
            }

            let dst_actual_hash = dst_actual_hash.finish();
            if dst_hash != &dst_actual_hash {
                return Err(InstallError::WrongHash {
                    expected: hex::encode(dst_hash),
                    actual: hex::encode(dst_actual_hash),
                });
            }
        }
        progress.net.value += response_net_counter.swap(0, atomic::Ordering::Relaxed);
    }

    if let Some(old_install_dir) = old_install_dir.as_ref() {
        progress.emit_msg(app, "Copying save files")?;
        for save_dir in ["Config", "SaveGames"] {
            let path = PathBuf::from("PackWisely/Saved/").join(save_dir);
            copy_dir(&old_install_dir.join(&path), &new_install_dir.join(&path)).await?;
        }
    }

    progress.emit_msg(app, "Removing old files")?;
    if let Some(old_install_dir) = old_install_dir.as_ref() {
        for file in new_patch_mf.stale_files.iter() {
            tokio::fs::remove_file(&old_install_dir.join(file)).await?;
        }
    }
    for file in files_to_remove.iter() {
        tokio::fs::remove_file(file).await?;
    }

    progress.emit(app)?;

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
