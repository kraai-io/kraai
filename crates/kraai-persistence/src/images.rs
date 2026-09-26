use std::fmt::Write;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, bail, ensure, eyre};
use image::{ImageDecoder, ImageFormat, ImageReader, Limits};
use kraai_types::image::{ImageAttachment, MAX_IMAGE_BYTES, MAX_IMAGE_PIXELS};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tokio::sync::Semaphore;

static IMAGE_DECODERS: Semaphore = Semaphore::const_new(2);

#[derive(Debug, Clone)]
pub struct FileImageStore {
    directory: PathBuf,
}

impl FileImageStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            directory: data_dir.join("images"),
        }
    }

    pub async fn import(&self, bytes: Vec<u8>) -> Result<ImageAttachment> {
        ensure!(
            bytes.len() <= MAX_IMAGE_BYTES,
            "Image exceeds {MAX_IMAGE_BYTES} bytes"
        );
        let permit = IMAGE_DECODERS.acquire().await?;
        let (attachment, bytes) = tokio::task::spawn_blocking(move || {
            let attachment = inspect(&bytes)?;
            drop(permit);
            Ok::<_, color_eyre::Report>((attachment, bytes))
        })
        .await
        .context("Image validation task failed")??;
        crate::atomic_write(&self.directory.join(&attachment.id), &bytes).await?;
        if let Some(parent) = self.directory.parent() {
            crate::sync_parent_directory(parent).await?;
        }
        Ok(attachment)
    }

    pub async fn read(&self, attachment: &ImageAttachment) -> Result<Vec<u8>> {
        attachment.validate().map_err(|error| eyre!(error))?;
        let path = self.directory.join(&attachment.id);
        let mut options = tokio::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
        let file = options
            .open(&path)
            .await
            .with_context(|| format!("Failed to read image {}", attachment.id))?;
        let metadata = file.metadata().await?;
        ensure!(metadata.is_file(), "Stored image is not a regular file");
        ensure!(
            metadata.len() == attachment.byte_length,
            "Stored image length does not match attachment"
        );
        let mut bytes = Vec::new();
        file.take(MAX_IMAGE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .await?;
        let expected = attachment.clone();
        let permit = IMAGE_DECODERS.acquire().await?;
        let bytes = tokio::task::spawn_blocking(move || {
            ensure!(
                inspect(&bytes)? == expected,
                "Stored image does not match attachment"
            );
            drop(permit);
            Ok::<_, color_eyre::Report>(bytes)
        })
        .await
        .context("Stored image validation task failed")??;
        Ok(bytes)
    }
}

fn inspect(bytes: &[u8]) -> Result<ImageAttachment> {
    ensure!(
        bytes.len() <= MAX_IMAGE_BYTES,
        "Image exceeds {MAX_IMAGE_BYTES} bytes"
    );
    let format = image::guess_format(bytes).context("Invalid image")?;
    let mime_type = match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        _ => bail!("Only PNG and JPEG images are supported"),
    };
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_PIXELS as u32);
    limits.max_image_height = Some(MAX_IMAGE_PIXELS as u32);
    limits.max_alloc = Some(MAX_IMAGE_PIXELS * 8);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let decoder = reader.into_decoder().context("Invalid image header")?;
    let (width, height) = decoder.dimensions();
    ensure!(
        width > 0 && height > 0 && u64::from(width) * u64::from(height) <= MAX_IMAGE_PIXELS,
        "Image must contain 1 to {MAX_IMAGE_PIXELS} pixels"
    );
    ensure!(
        decoder.total_bytes() <= MAX_IMAGE_PIXELS * 8,
        "Decoded image exceeds memory limit"
    );
    let decoded_length = usize::try_from(decoder.total_bytes())?;
    let mut decoded = vec![0; decoded_length];
    decoder
        .read_image(&mut decoded)
        .context("Invalid image data")?;
    let mut id = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        write!(id, "{byte:02x}")?;
    }
    Ok(ImageAttachment {
        id,
        mime_type: mime_type.to_string(),
        width,
        height,
        byte_length: bytes.len() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(format: ImageFormat) -> Result<Vec<u8>> {
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 3).write_to(&mut bytes, format)?;
        Ok(bytes.into_inner())
    }

    #[tokio::test]
    async fn images_survive_store_recreation() -> Result<()> {
        let directory = tempfile::tempdir()?;
        for format in [ImageFormat::Png, ImageFormat::Jpeg] {
            let bytes = fixture(format)?;
            let store = FileImageStore::new(directory.path());
            let attachment = store.import(bytes.clone()).await?;
            ensure!(attachment.width == 2 && attachment.height == 3);
            ensure!(store.import(bytes.clone()).await? == attachment);
            drop(store);
            let store = FileImageStore::new(directory.path());
            ensure!(store.read(&attachment).await? == bytes);
        }
        Ok(())
    }

    #[tokio::test]
    async fn rejects_invalid_and_oversized_images() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = FileImageStore::new(directory.path());
        ensure!(store.import(b"not an image".to_vec()).await.is_err());
        ensure!(store.import(b"GIF89a".to_vec()).await.is_err());
        ensure!(store.import(vec![0; MAX_IMAGE_BYTES + 1]).await.is_err());
        let mut bytes = fixture(ImageFormat::Png)?;
        bytes.truncate(bytes.len() / 2);
        let result = store.import(bytes).await;
        ensure!(result.is_err());
        ensure!(!store.directory.exists());
        Ok(())
    }

    #[tokio::test]
    async fn rejects_corrupt_bytes_metadata_and_paths() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = FileImageStore::new(directory.path());
        let attachment = store.import(fixture(ImageFormat::Png)?).await?;
        let mut changed = attachment.clone();
        changed.width += 1;
        ensure!(store.read(&changed).await.is_err());
        changed = attachment.clone();
        changed.mime_type = "image/jpeg".to_string();
        ensure!(store.read(&changed).await.is_err());
        changed.id = "../outside".to_string();
        ensure!(store.read(&changed).await.is_err());
        ensure!(
            serde_json::from_value::<ImageAttachment>(serde_json::to_value(&changed)?).is_err()
        );
        changed = attachment.clone();
        changed.id = "0".repeat(64);
        tokio::fs::copy(
            store.directory.join(&attachment.id),
            store.directory.join(&changed.id),
        )
        .await?;
        ensure!(store.read(&changed).await.is_err());
        tokio::fs::write(store.directory.join(&attachment.id), b"corruption").await?;
        ensure!(store.read(&attachment).await.is_err());
        tokio::fs::remove_file(store.directory.join(&attachment.id)).await?;
        ensure!(store.read(&attachment).await.is_err());
        Ok(())
    }

    #[test]
    fn rejects_images_above_pixel_limit_before_decoding() -> Result<()> {
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::new_luma8(4001, 4000).write_to(&mut bytes, ImageFormat::Png)?;
        ensure!(inspect(bytes.get_ref()).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_fifo_and_symlink_blobs_without_blocking() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = FileImageStore::new(directory.path());
        let attachment = store.import(fixture(ImageFormat::Png)?).await?;
        let path = store.directory.join(&attachment.id);
        tokio::fs::remove_file(&path).await?;
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &path,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            0,
        )?;
        let read = tokio::time::timeout(std::time::Duration::from_secs(1), store.read(&attachment))
            .await?;
        ensure!(read.is_err());
        tokio::fs::remove_file(&path).await?;
        let source = directory.path().join("source.png");
        tokio::fs::write(&source, fixture(ImageFormat::Png)?).await?;
        std::os::unix::fs::symlink(source, path)?;
        ensure!(store.read(&attachment).await.is_err());
        Ok(())
    }
}
