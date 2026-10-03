#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Thumbnail {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

pub fn process_image_payload(img_data: &[u8]) -> (Option<(u32, u32)>, Option<Thumbnail>) {
    const SIZE: u32 = 64;
    match image::load_from_memory(img_data) {
        Ok(dyn_img) => {
            let dimensions = (dyn_img.width(), dyn_img.height());
            let thumb = dyn_img.thumbnail(SIZE, SIZE).to_rgba8();
            let thumbnail = Some(Thumbnail {
                width: thumb.width() as usize,
                height: thumb.height() as usize,
                rgba: thumb.into_raw(),
            });
            (Some(dimensions), thumbnail)
        }
        Err(e) => {
            eprintln!("Failed to load image: {e}");
            (None, None)
        }
    }
}
