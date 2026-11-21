#[derive(Clone, Debug)]
pub struct Palette {
    pub bg: [u8; 3],
    pub fg: [u8; 3],
    pub prompt: [u8; 3],
    pub error: [u8; 3],
    pub panel: [u8; 3],
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            bg: [30, 30, 30],
            fg: [204, 204, 204],
            prompt: [100, 200, 100],
            error: [255, 100, 100],
            panel: [45, 45, 45],
        }
    }
}

pub fn color32(rgb: [u8; 3]) -> eframe::egui::Color32 {
    eframe::egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}
