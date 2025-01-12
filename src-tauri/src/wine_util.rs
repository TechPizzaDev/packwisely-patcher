use std::string::FromUtf8Error;

#[derive(thiserror::Error, Debug)]
pub(crate) enum WineError {
    #[error("unsupported operating system")]
    UnsupportedOS,
    #[error("could not find command")]
    NotFound,
    #[error("IO error")]
    Io(#[from] std::io::Error),
    #[error("failed to decode command path")]
    InvalidPath(#[from] FromUtf8Error),
}

pub(crate) fn get_wine_path() -> Result<String, WineError> {
    if std::env::consts::FAMILY == "unix" {
        let output = std::process::Command::new("which").arg("wine").output()?;
        if output.status.success() {
            Ok(String::from_utf8(output.stdout)?)
        } else {
            Err(WineError::NotFound)
        }
    } else {
        Err(WineError::UnsupportedOS)
    }
}
