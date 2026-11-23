use super::*;

#[test]
fn missing_image_is_empty_but_clipboard_failures_remain_errors() {
    assert!(
        encode_clipboard_result(Err(arboard::Error::ContentNotAvailable))
            .unwrap()
            .is_none()
    );
    assert!(encode_clipboard_result(Err(arboard::Error::ClipboardOccupied)).is_err());
    assert!(
        encode_clipboard_result(Ok(arboard::ImageData {
            width: 1,
            height: 1,
            bytes: vec![255; 4].into(),
        }))
        .unwrap()
        .is_some()
    );
}

#[test]
fn encodes_clipboard_pixels_and_rejects_invalid_dimensions() {
    let bytes = encode(arboard::ImageData {
        width: 2,
        height: 3,
        bytes: vec![255; 24].into(),
    })
    .unwrap();
    let decoded = image::load_from_memory(&bytes).unwrap();
    assert!(decoded.width() == 2 && decoded.height() == 3);
    for (width, height) in [(0, 1), (usize::MAX, 1), (4001, 4000), (2, 2)] {
        assert!(
            encode(arboard::ImageData {
                width,
                height,
                bytes: Vec::new().into()
            })
            .is_err()
        );
    }
}
