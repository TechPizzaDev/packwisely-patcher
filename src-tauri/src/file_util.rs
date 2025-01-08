use std::{fs::FileType, path::PathBuf};

use async_stream::try_stream;
use futures::Stream;
use tokio::fs::{self, DirEntry};

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
