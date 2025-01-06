use std::{error::Error, path::PathBuf};

use fast_rsync::SignatureOptions;
use rfd::AsyncFileDialog;
use tauri::{AppHandle, Emitter};
use tauri_plugin_dialog::DialogExt;
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt},
};

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command]
async fn install(app: AppHandle) -> String {
    let input_path = AsyncFileDialog::from(app.dialog().file()).pick_file().await;
    let output_path = AsyncFileDialog::from(app.dialog().file()).save_file().await;
    if let Some(input_file) = input_path {
        if let Some(out_file) = output_path {
            do_install(
                app,
                input_file.path().to_path_buf(),
                out_file.path().to_path_buf(),
            )
            .await
            .unwrap();
        }
    }

    format!("Installed {}", 123)
}

async fn do_install(
    app: AppHandle,
    source_file: PathBuf,
    sig_file: PathBuf,
) -> Result<(), Box<dyn Error + 'static>> {
    let mut sig_fs = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&sig_file)
        .await?;

    let sig = if sig_fs.metadata().await?.len() == 0 {
        let mut source_fs = File::open(&source_file).await?;
        fast_rsync::Signature::calculate(
            &mut source_fs,
            &mut sig_fs,
            &SignatureOptions::new(
                fast_rsync::RollingHashType::Rollsum,
                fast_rsync::CryptoHashType::Md4,
                1024,
                16,
            ),
        )
        .await?
    } else {
        fast_rsync::Signature::deserialize(&mut sig_fs).await?
    };
    sig_fs.seek(std::io::SeekFrom::Start(0)).await?;

    let sig_len = sig_fs.metadata().await?.len();
    let mut sig_buf = Vec::with_capacity(sig_len as usize);
    sig_fs.read_to_end(&mut sig_buf).await?;

    let index = sig.index(&sig_buf);

    app.emit("install-finished", format!("sig len: {}", sig_len))
        .unwrap();

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![greet, install])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
