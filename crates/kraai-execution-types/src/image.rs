use serde::{Deserialize, Deserializer, Serialize};

pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_IMAGE_PIXELS: u64 = 16_000_000;
pub const MAX_IMAGE_ATTACHMENTS: usize = 8;
pub const MAX_REQUEST_IMAGES: usize = 32;
pub const MAX_REQUEST_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageAttachment {
    #[serde(deserialize_with = "deserialize_id")]
    pub id: String,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    #[cfg_attr(feature = "typescript", ts(type = "number"))]
    pub byte_length: u64,
}

impl ImageAttachment {
    pub fn validate(&self) -> Result<(), String> {
        validate_image_id(&self.id)?;
        if !matches!(self.mime_type.as_str(), "image/png" | "image/jpeg") {
            return Err("Only PNG and JPEG images are supported".to_string());
        }
        if self.width == 0
            || self.height == 0
            || u64::from(self.width) * u64::from(self.height) > MAX_IMAGE_PIXELS
        {
            return Err(format!("Image must contain 1 to {MAX_IMAGE_PIXELS} pixels"));
        }
        if self.byte_length == 0 || self.byte_length > MAX_IMAGE_BYTES as u64 {
            return Err(format!("Image must contain 1 to {MAX_IMAGE_BYTES} bytes"));
        }
        Ok(())
    }
}

pub fn validate_image_id(id: &str) -> Result<(), String> {
    if id.len() != 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("Image id must be a lowercase SHA-256 digest".to_string());
    }
    Ok(())
}

fn deserialize_id<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let id = String::deserialize(deserializer)?;
    validate_image_id(&id).map_err(serde::de::Error::custom)?;
    Ok(id)
}
