//! Minimal egui 0.29 CPU painter. Consumes egui's tessellated/antialiased
//! triangle meshes (so rounded cards, shadows, glyphs and images use exactly
//! the same geometry as the GPU backends), then presents via softbuffer.
use eframe::egui::{self, Color32, TextureId};
use std::collections::HashMap;

struct Texture {
    size: [usize; 2],
    pixels: Vec<Color32>,
    linear: bool,
}
#[derive(Default)]
pub struct Painter {
    textures: HashMap<TextureId, Texture>,
    pixels: Vec<u32>,
    size: [usize; 2],
}
impl Painter {
    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }
    pub fn screenshot(&self) -> egui::ColorImage {
        egui::ColorImage {
            size: self.size,
            pixels: self
                .pixels
                .iter()
                .map(|p| Color32::from_rgb((p >> 16) as u8, (p >> 8) as u8, *p as u8))
                .collect(),
        }
    }
    pub fn paint(
        &mut self,
        size: [usize; 2],
        ppp: f32,
        jobs: &[egui::ClippedPrimitive],
        delta: &egui::TexturesDelta,
        clear: Color32,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(ppp.is_finite() && ppp > 0.0, "invalid software scale");
        let len = size[0]
            .checked_mul(size[1])
            .ok_or_else(|| anyhow::anyhow!("software framebuffer size overflow"))?;
        for (id, update) in &delta.set {
            self.update_texture(*id, update)?;
        }
        self.size = size;
        // Release oversized buffers after restoring from a much larger monitor.
        if len < self.pixels.capacity() / 2 {
            self.pixels = Vec::with_capacity(len);
        }
        self.pixels.resize(len, 0);
        let color = ((clear.r() as u32) << 16) | ((clear.g() as u32) << 8) | clear.b() as u32;
        self.pixels.fill(color);
        for job in jobs {
            let egui::epaint::Primitive::Mesh(mesh) = &job.primitive else {
                anyhow::bail!("GPU paint callbacks are not supported by the software renderer");
            };
            let texture = self
                .textures
                .get(&mesh.texture_id)
                .ok_or_else(|| anyhow::anyhow!("missing software texture {:?}", mesh.texture_id))?;
            let clip = job.clip_rect;
            if !clip.is_positive() {
                continue;
            }
            let min_x = ((clip.min.x * ppp - 0.5).ceil().max(0.0) as usize).min(size[0]);
            let min_y = ((clip.min.y * ppp - 0.5).ceil().max(0.0) as usize).min(size[1]);
            let max_x = ((clip.max.x * ppp - 0.5).ceil().max(0.0) as usize).min(size[0]);
            let max_y = ((clip.max.y * ppp - 0.5).ceil().max(0.0) as usize).min(size[1]);
            for indices in mesh.indices.chunks_exact(3) {
                let vertices = [
                    mesh.vertices[indices[0] as usize],
                    mesh.vertices[indices[1] as usize],
                    mesh.vertices[indices[2] as usize],
                ];
                triangle(
                    &mut self.pixels,
                    size,
                    ppp,
                    vertices,
                    texture,
                    [min_x, min_y, max_x, max_y],
                );
            }
        }
        for id in &delta.free {
            self.textures.remove(id);
        }
        Ok(())
    }
    fn update_texture(
        &mut self,
        id: TextureId,
        update: &egui::epaint::ImageDelta,
    ) -> anyhow::Result<()> {
        let size = update.image.size();
        anyhow::ensure!(size[0] > 0 && size[1] > 0, "empty software texture");
        let pixels: Vec<Color32> = match &update.image {
            egui::ImageData::Color(image) => image.pixels.clone(),
            egui::ImageData::Font(image) => image.srgba_pixels(None).collect(),
        };
        let linear = update.options.magnification == egui::TextureFilter::Linear;
        if let Some([x, y]) = update.pos {
            let texture = self
                .textures
                .get_mut(&id)
                .ok_or_else(|| anyhow::anyhow!("partial update of unknown texture"))?;

            anyhow::ensure!(
                x + size[0] <= texture.size[0] && y + size[1] <= texture.size[1],
                "software texture update out of bounds"
            );
            for row in 0..size[1] {
                let dest = (y + row) * texture.size[0] + x;
                texture.pixels[dest..dest + size[0]]
                    .copy_from_slice(&pixels[row * size[0]..(row + 1) * size[0]]);
            }
            texture.linear = linear;
        } else {
            self.textures.insert(
                id,
                Texture {
                    size,
                    pixels,
                    linear,
                },
            );
        }
        Ok(())
    }
}
fn edge(a: egui::Pos2, b: egui::Pos2, p: egui::Pos2) -> f32 {
    (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
}
fn triangle(
    dst: &mut [u32],
    size: [usize; 2],
    ppp: f32,
    mut v: [egui::epaint::Vertex; 3],
    tex: &Texture,
    clip: [usize; 4],
) {
    let mut pos = v.map(|p| p.pos * ppp);
    let mut area = edge(pos[0], pos[1], pos[2]);
    if !area.is_finite() || area.abs() < 0.0001 {
        return;
    }
    if area < 0.0 {
        v.swap(1, 2);
        pos.swap(1, 2);
        area = -area;
    }
    let min_y = ((pos.iter().map(|p| p.y).fold(f32::INFINITY, f32::min) - 0.5)
        .ceil()
        .max(0.0) as usize)
        .max(clip[1]);
    let max_y = ((pos.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max) - 0.5)
        .ceil()
        .max(0.0) as usize)
        .min(clip[3]);
    if max_y <= min_y {
        return;
    }
    let edges = [(pos[1], pos[2]), (pos[2], pos[0]), (pos[0], pos[1])];
    let step_x = edges.map(|(a, b)| -(b.y - a.y));
    let inv = 1.0 / area;
    let colors = v.map(|p| p.color.to_array());
    let uniform_uv = v.iter().all(|p| p.uv == v[0].uv);
    let texture_color = uniform_uv.then(|| sample(tex, v[0].uv));
    let solid = v.iter().all(|p| p.color == v[0].color) && uniform_uv;
    let solid_color = solid.then(|| modulate(texture_color.unwrap(), colors[0].map(|c| c as f32)));
    if solid_color.is_some_and(|c| c == [0.0; 4]) {
        return;
    }
    let opaque = solid_color.filter(|c| c[3] >= 254.9).map(|c| blend(0, c));
    // Scanline spans avoid walking a rectangle twice for each large triangle.
    // Shared diagonal coverage is half-open [left,right), matching the top-left
    // raster rule. Opaque uniform spans are a bulk fill, not a per-pixel blend.
    for y in min_y..max_y {
        let py = y as f32 + 0.5;
        let mut left = f32::INFINITY;
        let mut right = f32::NEG_INFINITY;
        for (a, b) in edges {
            if (a.y <= py && py < b.y) || (b.y <= py && py < a.y) {
                let x = a.x + (py - a.y) * (b.x - a.x) / (b.y - a.y);
                left = left.min(x);
                right = right.max(x);
            }
        }
        if !left.is_finite() || !right.is_finite() {
            continue;
        }
        let min_x = ((left - 0.5).ceil().max(0.0) as usize).max(clip[0]);
        let max_x = ((right - 0.5).ceil().max(0.0) as usize).min(clip[2]);
        if max_x <= min_x {
            continue;
        }
        let span = &mut dst[y * size[0] + min_x..y * size[0] + max_x];
        if let Some(color) = opaque {
            span.fill(color);
            continue;
        }
        if let Some(color) = solid_color {
            for p in span {
                *p = blend(*p, color);
            }
            continue;
        }
        let start = egui::pos2(min_x as f32 + 0.5, py);
        let mut e = edges.map(|(a, b)| edge(a, b, start));
        for pixel in span {
            let weights = e.map(|s| s * inv);
            let texture = if let Some(color) = texture_color {
                color
            } else {
                let uv = v[0].uv * weights[0]
                    + v[1].uv.to_vec2() * weights[1]
                    + v[2].uv.to_vec2() * weights[2];
                sample(tex, uv)
            };
            let color = std::array::from_fn(|i| {
                colors[0][i] as f32 * weights[0]
                    + colors[1][i] as f32 * weights[1]
                    + colors[2][i] as f32 * weights[2]
            });
            *pixel = blend(*pixel, modulate(texture, color));
            for i in 0..3 {
                e[i] += step_x[i];
            }
        }
    }
}
fn modulate(texture: [f32; 4], color: [f32; 4]) -> [f32; 4] {
    std::array::from_fn(|i| texture[i] * color[i] / 255.0)
}
fn blend(dst: u32, src: [f32; 4]) -> u32 {
    if src[3] >= 254.9 {
        return ((src[0].round() as u32).min(255) << 16)
            | ((src[1].round() as u32).min(255) << 8)
            | (src[2].round() as u32).min(255);
    }
    let inverse = 1.0 - src[3] / 255.0;
    let r = (src[0] + ((dst >> 16) & 255) as f32 * inverse)
        .round()
        .clamp(0.0, 255.0) as u32;
    let g = (src[1] + ((dst >> 8) & 255) as f32 * inverse)
        .round()
        .clamp(0.0, 255.0) as u32;
    let b = (src[2] + (dst & 255) as f32 * inverse)
        .round()
        .clamp(0.0, 255.0) as u32;
    (r << 16) | (g << 8) | b
}
fn sample(tex: &Texture, uv: egui::Pos2) -> [f32; 4] {
    let x = (uv.x * tex.size[0] as f32 - 0.5).clamp(0.0, (tex.size[0] - 1) as f32);
    let y = (uv.y * tex.size[1] as f32 - 0.5).clamp(0.0, (tex.size[1] - 1) as f32);
    if !tex.linear {
        return tex.pixels[y.round() as usize * tex.size[0] + x.round() as usize]
            .to_array()
            .map(|c| c as f32);
    }
    let x0 = x as usize;
    let y0 = y as usize;
    let x1 = (x0 + 1).min(tex.size[0] - 1);
    let y1 = (y0 + 1).min(tex.size[1] - 1);
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let c = [
        tex.pixels[y0 * tex.size[0] + x0],
        tex.pixels[y0 * tex.size[0] + x1],
        tex.pixels[y1 * tex.size[0] + x0],
        tex.pixels[y1 * tex.size[0] + x1],
    ]
    .map(|p| p.to_array());
    std::array::from_fn(|i| {
        (c[0][i] as f32 * (1.0 - fx) + c[1][i] as f32 * fx) * (1.0 - fy)
            + (c[2][i] as f32 * (1.0 - fx) + c[3][i] as f32 * fx) * fy
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn quad(color: Color32) -> egui::ClippedPrimitive {
        let mut mesh = egui::Mesh::with_texture(TextureId::Managed(0));
        mesh.add_colored_rect(
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(4.0, 4.0)),
            color,
        );
        egui::ClippedPrimitive {
            clip_rect: egui::Rect::EVERYTHING,
            primitive: egui::epaint::Primitive::Mesh(mesh),
        }
    }
    fn white() -> egui::TexturesDelta {
        egui::TexturesDelta {
            set: vec![(
                TextureId::Managed(0),
                egui::epaint::ImageDelta::full(
                    egui::ColorImage::new([1, 1], Color32::WHITE),
                    egui::TextureOptions::LINEAR,
                ),
            )],
            free: vec![],
        }
    }
    #[test]
    fn shared_triangle_diagonal_is_not_blended_twice() {
        let mut p = Painter::default();
        p.paint(
            [4, 4],
            1.0,
            &[quad(Color32::from_rgba_premultiplied(128, 0, 0, 128))],
            &white(),
            Color32::BLACK,
        )
        .unwrap();
        assert!(
            p.pixels().iter().all(|v| *v == 0x00800000),
            "shared edges must be covered exactly once: {:?}",
            p.pixels()
        );
    }
    #[test]
    fn clipping_partial_updates_and_texture_free() {
        let mut p = Painter::default();
        let mut q = quad(Color32::WHITE);
        q.clip_rect = egui::Rect::from_min_max(egui::pos2(1.0, 1.0), egui::pos2(3.0, 3.0));
        p.paint([4, 4], 1.0, &[q.clone()], &white(), Color32::BLACK)
            .unwrap();
        assert_eq!(p.pixels().iter().filter(|v| **v == 0xFFFFFF).count(), 4);
        let delta = egui::TexturesDelta {
            set: vec![(
                TextureId::Managed(0),
                egui::epaint::ImageDelta::partial(
                    [0, 0],
                    egui::ColorImage::new([1, 1], Color32::RED),
                    egui::TextureOptions::LINEAR,
                ),
            )],
            free: vec![TextureId::Managed(0)],
        };
        p.paint([4, 4], 1.0, &[q], &delta, Color32::BLACK).unwrap();
        assert_eq!(p.pixels().iter().filter(|v| **v == 0xFF0000).count(), 4);
        assert!(p.textures.is_empty());
    }
    #[test]
    fn bilinear_texel_centers_and_fractional_scale() {
        let tex = Texture {
            size: [2, 1],
            pixels: vec![Color32::BLACK, Color32::WHITE],
            linear: true,
        };
        assert_eq!(
            sample(&tex, egui::pos2(0.5, 0.5)),
            [127.5, 127.5, 127.5, 255.0]
        );
        let mut p = Painter::default();
        p.paint(
            [5, 5],
            1.25,
            &[quad(Color32::WHITE)],
            &white(),
            Color32::BLACK,
        )
        .unwrap();
        assert!(p.pixels().iter().all(|v| *v == 0xFFFFFF));
    }
    #[test]
    fn shrinking_after_large_window_releases_framebuffer_capacity() {
        let mut p = Painter::default();
        p.paint([1000, 1000], 1.0, &[], &Default::default(), Color32::WHITE)
            .unwrap();
        p.paint([10, 10], 1.0, &[], &Default::default(), Color32::WHITE)
            .unwrap();
        assert!(p.pixels.capacity() <= 100);
    }
    #[test]
    fn invalid_partial_texture_updates_are_reported() {
        let mut p = Painter::default();
        let delta = egui::TexturesDelta {
            set: vec![(
                TextureId::Managed(1),
                egui::epaint::ImageDelta::partial(
                    [0, 0],
                    egui::ColorImage::new([1, 1], Color32::RED),
                    egui::TextureOptions::LINEAR,
                ),
            )],
            free: vec![],
        };
        assert!(p.paint([4, 4], 1.0, &[], &delta, Color32::BLACK).is_err());
    }
}
