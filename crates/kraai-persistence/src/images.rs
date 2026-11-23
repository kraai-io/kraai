use std::fmt::Write;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, bail, ensure, eyre};
use image::{ImageDecoder, ImageFormat, ImageReader, Limits};
use kraai_execution_types::image::{ImageAttachment, MAX_IMAGE_BYTES, MAX_IMAGE_PIXELS};
use kraai_io::fs::{FinalSymlinkPolicy, open_regular_file_async};
use kraai_io::read::{ReadLimitError, read_bounded_async};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

static IMAGE_DECODERS: Semaphore = Semaphore::const_new(2);

#[derive(Debug, Clone)]
pub struct FileImageStore {
    root: PathBuf,
    directory: PathBuf,
}

impl FileImageStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            root: data_dir.to_path_buf(),
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
        kraai_io::fs::atomic_replace_in_async(
            &self.root,
            &self.directory.join(&attachment.id),
            &bytes,
        )
        .await?
        .into_result()?;
        Ok(attachment)
    }

    pub async fn read(&self, attachment: &ImageAttachment) -> Result<Vec<u8>> {
        attachment.validate().map_err(|error| eyre!(error))?;
        let path = self.directory.join(&attachment.id);
        let file = open_regular_file_async(&path, FinalSymlinkPolicy::Reject)
            .await
            .with_context(|| format!("Failed to read image {}", attachment.id))?;
        let metadata = file.metadata().await?;
        ensure!(
            metadata.len() == attachment.byte_length,
            "Stored image length does not match attachment"
        );
        let bytes = read_bounded_async(file, MAX_IMAGE_BYTES as u64)
            .await
            .map_err(|error| match error {
                ReadLimitError::Io(error) => color_eyre::Report::from(error),
                ReadLimitError::Exceeded { .. } => eyre!("Image exceeds {MAX_IMAGE_BYTES} bytes"),
            })?;
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
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "every image format other than PNG and JPEG is rejected"
    )]
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
mod tests;
