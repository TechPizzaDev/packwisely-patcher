use std::{
    fs::FileType,
    path::{Path, PathBuf, StripPrefixError},
};

use async_stream::try_stream;
use futures::{pin_mut, Stream, StreamExt};
use tokio::fs::{self, DirEntry, File};

pub fn visit_stream(
    path: impl Into<PathBuf>,
) -> impl Stream<Item = std::io::Result<(FileType, DirEntry)>> {
    try_stream! {
        let mut to_visit = vec![path.into()];
        while let Some(path) = to_visit.pop() {
            let mut dir = fs::read_dir(path).await?;
            while let Some(child) = dir.next_entry().await? {
                let file_type = child.file_type().await?;
                if file_type.is_dir() {
                    to_visit.push(child.path());
                }
                yield (file_type, child);
            }
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum CopyError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("strip prefix: {0}")]
    StripPrefix(#[from] StripPrefixError),
    #[error("failed to get parent")]
    Orphan,
}

pub async fn copy_dir(src_dir: &PathBuf, dst_dir: &PathBuf) -> Result<(), CopyError> {
    let entries = visit_stream(&src_dir);
    pin_mut!(entries);
    while let Some((_, entry)) = entries.next().await.transpose()? {
        let src_path = entry.path();
        let relative_path = src_path.strip_prefix(&src_dir)?;
        let dst_path = dst_dir.join(relative_path);

        let dst_parent = dst_path.parent().ok_or(CopyError::Orphan)?;
        tokio::fs::create_dir(dst_parent).await?;

        File::create_new(&dst_path).await?;
        tokio::fs::copy(src_path, dst_path).await?;
    }
    Ok(())
}
