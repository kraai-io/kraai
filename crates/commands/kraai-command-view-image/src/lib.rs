#![forbid(unsafe_code)]

use std::path::Path;

use kraai_command_core::{command_error, declare_kraai_command};
use kraai_io::fs::{FinalSymlinkPolicy, open_regular_file};
use kraai_io::read::{ReadLimitError, read_bounded};
use kraai_types::image::MAX_IMAGE_BYTES;
use nu_engine::CallExt;
use nu_protocol::{Category, IntoPipelineData, Signature, SyntaxShape, Type, Value, record};

declare_kraai_command! {
    /// Attaches a snapshot of an image to the current script result.
    pub struct ViewImageCommand;
    metadata: kraai_command_catalog::VIEW_IMAGE;
    signature: Signature::build(Self::METADATA.name)
        .optional("path", SyntaxShape::String, "PNG or JPEG image path.")
        .named("attachment", SyntaxShape::String, "Reopen an image attachment from this session by id.", None)
        .input_output_types(vec![(Type::Nothing, Type::Record(Default::default()))])
        .category(Category::Experimental);
    run: |context, engine_state, stack, call, _input| {
        let path: Option<String> = call.opt(engine_state, stack, 0)?;
        let existing: Option<String> = call.get_flag(engine_state, stack, "attachment")?;
        let image = match (path, existing) {
            (Some(path), None) => {
                let cwd = engine_state.cwd(Some(stack))?;
                let bytes = read_image(cwd.as_ref(), Path::new(&path)).map_err(|error| {
                    command_error("Unable to read image", error.to_string(), call.head)
                })?;
                context.images().attach(bytes)
            }
            (None, Some(id)) => context.images().attach_existing(id),
            _ => return Err(command_error(
                "Invalid image request",
                "Supply either an image path or --attachment <id>",
                call.head,
            )),
        }.map_err(|error| command_error("Unable to attach image", error, call.head))?;
        Ok(Value::record(
            record! {
                "id" => Value::string(image.id, call.head),
                "mime_type" => Value::string(image.mime_type, call.head),
                "width" => Value::int(i64::from(image.width), call.head),
                "height" => Value::int(i64::from(image.height), call.head),
            },
            call.head,
        ).into_pipeline_data())
    }
}

fn read_image(cwd: &Path, requested: &Path) -> std::io::Result<Vec<u8>> {
    let path = cwd.join(requested);
    let file = open_regular_file(&path, FinalSymlinkPolicy::Follow)?;
    let limit = MAX_IMAGE_BYTES as u64;
    if file.metadata()?.len() > limit {
        return Err(image_too_large());
    }
    read_bounded(file, limit).map_err(|error| match error {
        ReadLimitError::Io(error) => error,
        ReadLimitError::Exceeded { .. } => image_too_large(),
    })
}

fn image_too_large() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("image exceeds the {MAX_IMAGE_BYTES} byte limit"),
    )
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "image read tests propagate fixture errors and assert bounds"
)]
mod tests {
    use super::*;

    #[test]
    fn reads_binary_and_rejects_non_files_and_oversized_files() -> std::io::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("image.png");
        let expected = [0, 255, 128, 13, 10];
        std::fs::write(&path, expected)?;
        assert_eq!(
            read_image(directory.path(), Path::new("image.png"))?,
            expected
        );
        assert!(read_image(directory.path(), directory.path()).is_err());
        std::fs::File::create(&path)?.set_len(MAX_IMAGE_BYTES as u64 + 1)?;
        assert!(read_image(directory.path(), &path).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn rejects_fifo_without_waiting_for_a_writer() -> std::io::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("image.png");
        nix::unistd::mkfifo(
            &path,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )?;
        assert!(read_image(directory.path(), &path).is_err());
        Ok(())
    }
}
