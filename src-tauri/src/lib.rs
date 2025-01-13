mod file_util;
mod install;
mod wine_util;

use std::{
    collections::HashSet, fs::Permissions, os::unix::fs::PermissionsExt, path::PathBuf,
    process::Stdio,
};

use async_compat::{Compat, CompatExt};
use fast_rsync::{
    sum_hash::{Blake3Hash, SumHash},
    SignatureOptions,
};
use futures::{pin_mut, AsyncReadExt, StreamExt};
use install::do_install;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_with::base64::Base64;
use serde_with::serde_as;
use tauri::{AppHandle, Emitter, Listener, Manager};
use tauri_plugin_http::reqwest;
use tauri_plugin_updater::UpdaterExt;
use tokio::{
    fs::File,
    io::{AsyncReadExt as OtherAsyncReadExt, AsyncSeekExt, AsyncWriteExt},
};
use tokio_util::bytes::BytesMut;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn is_update_check_finished(app: AppHandle) -> bool {
    app.try_state::<UpdateCheckState>()
        .map(|state| state.finished)
        .unwrap_or(false)
}

#[tauri::command]
async fn install(app: AppHandle) -> Result<(), String> {
    let http_client = reqwest::Client::builder()
        .build()
        .map_err(|err| err.to_string())?;

    let install_dir = dirs::data_local_dir().ok_or("missing install dir")?;

    let exe_path = do_install(&app, &http_client, install_dir.join("PackWisely"))
        .await
        .map_err(|err| err.to_string())?;

    tokio::fs::set_permissions(&exe_path, Permissions::from_mode(0o770))
        .await
        .map_err(|err| err.to_string())?;

    std::process::Command::new(exe_path)
        .stdout(Stdio::inherit())
        .spawn()
        .map_err(|err| err.to_string())?;

    Ok(())
}

#[tauri::command]
async fn create_patch(
    app: AppHandle,
    out_dir: String,
    new_dir: String,
    old_dir: String,
    version: String,
) -> Result<CreatePatchResult, String> {
    let result = do_create_patch(
        app,
        out_dir.into(),
        new_dir.into(),
        (!old_dir.is_empty()).then(|| old_dir.into()),
        version,
    )
    .await
    .map_err(|err| err.to_string())?;

    Ok(result)
}

#[derive(Debug, Clone, Serialize)]
struct CreatePatchProgress {
    done_files: usize,
    total_files: usize,
    path: String,
}

#[derive(Debug, Clone, Serialize)]
struct CreatePatchResult {
    manifest: PatchManifest,
    patch_size: u64,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileManifest {
    path: String,
    len: u64,
    #[serde_as(as = "Base64")]
    hash: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum PatchManifestVersion {
    V1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PatchManifest {
    manifest_version: PatchManifestVersion,
    version: Version,
    previous_version: Option<Version>,
    new_files: Vec<FileManifest>,
    diff_files: Vec<FileManifest>,
    stale_files: Vec<String>,
}

async fn get_files(path: &PathBuf) -> std::io::Result<HashSet<PathBuf>> {
    let mut files = HashSet::new();
    let dir_visit = file_util::visit_stream(path);
    pin_mut!(dir_visit);
    while let Some((ty, entry)) = dir_visit.next().await.transpose()? {
        if ty.is_file() {
            files.insert(entry.path());
        }
    }
    Ok(files)
}

async fn do_create_patch(
    app: AppHandle,
    out_dir: PathBuf,
    new_dir: PathBuf,
    old_dir: Option<PathBuf>,
    version: String,
) -> anyhow::Result<CreatePatchResult> {
    let version = Version::parse(&version)?;

    let mut out_raw_tar = create_tar(&out_dir.join("raw.tar")).await?;
    let mut out_sig_tar = create_tar(&out_dir.join("sig.tar")).await?;
    let mut out_manifest_fs = File::create(out_dir.join("manifest.json")).await?;

    let diff_result = if let Some(old_dir) = old_dir {
        do_create_diff(&app, &out_dir, &new_dir, &old_dir).await?
    } else {
        let new_files = get_files(&new_dir).await?;
        DiffResult {
            prev_version: None,
            new_files,
            diff_files: vec![],
            stale_files: vec![],
            diff_size: 0,
        }
    };
    let diff_files = diff_result.diff_files;

    let mut progress = CreatePatchProgress {
        done_files: diff_files.len(),
        total_files: diff_files.len() + diff_result.new_files.len(),
        path: "".into(),
    };

    let mut new_mf_files = Vec::new();

    let mut write_buf = Vec::with_capacity(1024 * 16);
    let mut read_buf = BytesMut::with_capacity(1024 * 16);

    for file in diff_result.new_files.into_iter() {
        let relative_path = file.strip_prefix(&new_dir)?;

        progress.path = file.to_string_lossy().into();
        progress.emit(&app);

        let mut src_fs = File::open(&file).await?;
        let src_meta = src_fs.metadata().await?;

        let mut raw_header = async_tar::Header::new_gnu();
        raw_header.set_size(src_meta.len());
        out_raw_tar
            .append_data(&mut raw_header, relative_path, src_fs.compat_mut())
            .await?;
        src_fs.seek(std::io::SeekFrom::Start(0)).await?;

        fast_rsync::Signature::calculate(
            &mut src_fs,
            &mut write_buf,
            &SignatureOptions::new(
                fast_rsync::RollingHashType::RabinKarp,
                fast_rsync::CryptoHashType::Blake2,
                2048,
                8,
            ),
        )
        .await?;
        src_fs.seek(std::io::SeekFrom::Start(0)).await?;

        let mut sig_header = async_tar::Header::new_gnu();
        sig_header.set_size(write_buf.len().try_into().unwrap());
        out_sig_tar
            .append_data(&mut sig_header, relative_path, write_buf.as_slice())
            .await?;

        let mut hash = Blake3Hash::default();
        while src_fs.read_buf(&mut read_buf).await? != 0 {
            hash.update(&read_buf.split());
        }

        write_buf.clear();
        read_buf.clear();

        new_mf_files.push(FileManifest {
            path: relative_path.to_string_lossy().into(),
            len: src_meta.len(),
            hash: hash.finish(),
        });

        progress.done_files += 1;
        progress.emit(&app);
    }

    let manifest = PatchManifest {
        manifest_version: PatchManifestVersion::V1,
        version,
        previous_version: diff_result.prev_version,
        new_files: new_mf_files,
        diff_files,
        stale_files: diff_result.stale_files,
    };
    serde_json::to_writer(&mut write_buf, &manifest)?;
    out_manifest_fs.write_all(&mut write_buf).await?;

    let out_raw_fs = out_raw_tar.into_inner().await?;
    let out_raw_size = out_raw_fs.into_inner().metadata().await?.len();

    let out_sig_fs = out_sig_tar.into_inner().await?;
    let out_sig_size = out_sig_fs.into_inner().metadata().await?.len();

    let patch_size = diff_result.diff_size + out_sig_size + out_raw_size + write_buf.len() as u64;
    Ok(CreatePatchResult {
        manifest,
        patch_size,
    })
}

#[derive(Debug)]
struct DiffResult {
    prev_version: Option<Version>,
    new_files: HashSet<PathBuf>,
    diff_files: Vec<FileManifest>,
    stale_files: Vec<String>,
    diff_size: u64,
}

async fn do_create_diff(
    app: &AppHandle,
    out_dir: &PathBuf,
    new_dir: &PathBuf,
    old_dir: &PathBuf,
) -> anyhow::Result<DiffResult> {
    let old_patch_mf: PatchManifest = {
        let mut fs = File::open(out_dir.join("manifest.json")).await?;
        let mut str = String::new();
        fs.read_to_string(&mut str).await?;
        serde_json::from_str(&str)?
    };

    let old_sig_tar = open_tar(&old_dir.join("sig.tar")).await?;
    let mut out_diff_tar = create_tar(&out_dir.join("diff.tar")).await?;

    let mut new_files = get_files(&new_dir).await?;
    let mut diff_files = Vec::new();
    let mut stale_files = Vec::new();

    let mut sig_buf = Vec::new();
    let mut new_buf = Vec::new();
    let mut diff_buf = Vec::new();

    let mut progress = CreatePatchProgress {
        done_files: 0,
        total_files: new_files.len(),
        path: "".into(),
    };

    let mut old_entries = old_sig_tar.entries()?;
    while let Some(mut old_sig_entry) = old_entries.next().await.transpose()? {
        let relative_path = old_sig_entry.path()?.into_owned();
        let new_path = new_dir.join(&relative_path);

        if !new_files.remove(&new_path) {
            stale_files.push(relative_path.to_string_lossy().into());
            continue;
        }

        progress.path = new_path.to_string_lossy().into();
        progress.emit(app);

        old_sig_entry.read_to_end(&mut sig_buf).await?;
        let old_sig = fast_rsync::Signature::deserialize(&mut sig_buf.as_slice()).await?;
        let old_sig_index = old_sig.index(&sig_buf);

        let mut new_fs = File::open(&new_path).await?;
        new_fs.read_to_end(&mut new_buf).await?;
        fast_rsync::diff(&old_sig_index, &new_buf, &mut diff_buf)?;

        let mut diff_header = async_tar::Header::new_gnu();
        diff_header.set_size(diff_buf.len().try_into().unwrap());
        out_diff_tar
            .append_data(&mut diff_header, &relative_path, &mut diff_buf.as_slice())
            .await?;

        diff_files.push(FileManifest {
            path: relative_path.to_string_lossy().into(),
            len: new_buf.len() as u64,
            hash: Blake3Hash::default().update(&new_buf).finish(),
        });

        sig_buf.clear();
        new_buf.clear();
        diff_buf.clear();

        progress.done_files += 1;
        progress.emit(app);
    }

    let out_diff_fs = out_diff_tar.into_inner().await?;
    let out_diff_len = out_diff_fs.into_inner().metadata().await?.len();

    Ok(DiffResult {
        prev_version: Some(old_patch_mf.version),
        new_files,
        diff_files,
        stale_files,
        diff_size: out_diff_len,
    })
}

impl CreatePatchProgress {
    fn emit(&self, app: &AppHandle) {
        app.emit("create-patch-progress", self).unwrap();
    }
}

async fn create_tar(path: &PathBuf) -> std::io::Result<async_tar::Builder<Compat<File>>> {
    Ok(async_tar::Builder::new(File::create(path).await?.compat()))
}

async fn open_tar(path: &PathBuf) -> std::io::Result<async_tar::Archive<Compat<File>>> {
    Ok(async_tar::Archive::new(File::open(path).await?.compat()))
}

#[derive(Clone, Serialize, Deserialize)]
struct SingleInstancePayload {
    args: Vec<String>,
    cwd: String,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
            println!("{}, {argv:?}, {cwd}", app.package_info().name);

            app.emit("single-instance", SingleInstancePayload { args: argv, cwd })
                .unwrap();
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            is_update_check_finished,
            install,
            create_patch
        ])
        .setup(|app| {
            let app_handle = app.handle().clone();
            app.listen("single-instance", move |ev| {
                if let Ok(_) = serde_json::from_str::<SingleInstancePayload>(&ev.payload()) {
                    if let Some((_, window)) = app_handle.webview_windows().iter().next() {
                        _ = window.unminimize();
                        _ = window.set_focus();
                    }
                }
            });

            let app_handle = app.handle().clone();
            let update_join_handle = tauri::async_runtime::spawn(async move {
                update(app_handle).await.unwrap();
            });

            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(err) = update_join_handle.await {
                    println!("failed to update: {}", err);
                }
                app_handle.manage(UpdateCheckState { finished: true });
                app_handle.emit("update-check-finished", ()).unwrap();
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

struct UpdateCheckState {
    finished: bool,
}

async fn update(app: AppHandle) -> tauri_plugin_updater::Result<()> {
    println!("checking for update");

    if let Some(update) = app.updater()?.check().await? {
        let mut downloaded = 0;

        let bytes = update
            .download(
                |chunk_length, content_length| {
                    downloaded += chunk_length;
                    println!("downloaded {downloaded} from {content_length:?}");
                },
                || {
                    println!("download finished");
                },
            )
            .await?;

        update.install(bytes)?;

        println!("update installed");
        app.restart();
    } else {
        println!("up to date");
    }

    Ok(())
}
