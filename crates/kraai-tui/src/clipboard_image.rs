use std::io::Write;

use kraai_execution_types::image::{MAX_IMAGE_BYTES, MAX_IMAGE_PIXELS};
use kraai_runtime::{RuntimeError, RuntimeResult};

fn read() -> RuntimeResult<Option<Vec<u8>>> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|error| RuntimeError::unavailable(format!("Clipboard unavailable: {error}")))?;
    encode_clipboard_result(clipboard.get_image())
}

fn encode_clipboard_result(
    result: Result<arboard::ImageData<'_>, arboard::Error>,
) -> RuntimeResult<Option<Vec<u8>>> {
    match result {
        Ok(image) => encode(image).map(Some),
        Err(arboard::Error::ContentNotAvailable) => Ok(None),
        Err(error) => Err(RuntimeError::unavailable(format!(
            "Cannot read a clipboard image: {error}"
        ))),
    }
}

fn encode(image: arboard::ImageData<'_>) -> RuntimeResult<Vec<u8>> {
    let width = u32::try_from(image.width)
        .map_err(|_overflow| RuntimeError::invalid_argument("Image is too wide"))?;
    let height = u32::try_from(image.height)
        .map_err(|_overflow| RuntimeError::invalid_argument("Image is too tall"))?;
    let pixels = u64::from(width) * u64::from(height);
    if pixels == 0 || pixels > MAX_IMAGE_PIXELS {
        return Err(RuntimeError::invalid_argument(format!(
            "Image must contain 1 to {MAX_IMAGE_PIXELS} pixels"
        )));
    }
    let rgba = image::RgbaImage::from_raw(width, height, image.bytes.into_owned())
        .ok_or_else(|| RuntimeError::invalid_argument("Invalid clipboard image buffer"))?;
    let mut output = BoundedImage(Vec::new());
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut output),
        rgba.as_raw(),
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|error| {
        RuntimeError::unavailable(format!("Cannot encode clipboard image: {error}"))
    })?;
    Ok(output.0)
}

struct BoundedImage(Vec<u8>);

impl Write for BoundedImage {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_IMAGE_BYTES.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other(
                "Clipboard image exceeds the image size limit",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) const HELPER_ARG: &str = "--kraai-internal-clipboard-image";
pub(crate) const NO_IMAGE_EXIT_CODE: i32 = 2;

pub(crate) fn run_internal() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(HELPER_ARG)) || args.next().is_some() {
        return None;
    }
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(20));
        std::process::exit(124);
    });
    let result = prepare_helper().and_then(|()| read()).and_then(|bytes| {
        let Some(bytes) = bytes else {
            return Ok(NO_IMAGE_EXIT_CODE);
        };
        std::io::stdout()
            .lock()
            .write_all(&bytes)
            .map_err(|error| {
                RuntimeError::unavailable(format!("Cannot return clipboard image: {error}"))
            })?;
        Ok(0)
    });
    Some(match result {
        Ok(code) => code,
        Err(error) => {
            let _ = writeln!(std::io::stderr().lock(), "{error}");
            1
        }
    })
}

fn prepare_helper() -> RuntimeResult<()> {
    #[cfg(unix)]
    {
        use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
        #[cfg(target_os = "macos")]
        let resource = Resource::Data;
        #[cfg(not(target_os = "macos"))]
        let resource = Resource::As;
        let existing = getrlimit(resource);
        let maximum = existing.maximum.unwrap_or(u64::MAX).min(512 * 1024 * 1024);
        setrlimit(
            resource,
            Rlimit {
                current: Some(existing.current.unwrap_or(maximum).min(maximum)),
                maximum: Some(maximum),
            },
        )
        .map_err(|error| {
            RuntimeError::unavailable(format!("Cannot limit clipboard helper memory: {error}"))
        })?;
        setrlimit(
            Resource::Core,
            Rlimit {
                current: Some(0),
                maximum: Some(0),
            },
        )
        .map_err(|error| {
            RuntimeError::unavailable(format!(
                "Cannot disable clipboard helper core dumps: {error}"
            ))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
