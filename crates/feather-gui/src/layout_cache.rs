use eframe::egui;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

pub const MAX_ENTRIES: usize = 128;
pub const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Hash, PartialEq, Eq)]
pub struct GalleyKey {
    pub hash: u64,
    pub len: usize,
    pub width: u32,
    pub ppp: u32,
    pub atlas: usize,
    pub mono: bool,
    pub size_bits: u32,
    pub color: [u8; 4],
}

struct Entry {
    key: GalleyKey,
    galley: Arc<egui::Galley>,
    bytes: usize,
}

#[derive(Default)]
pub struct GalleyRing {
    entries: VecDeque<Entry>,
    bytes: usize,
    atlas: Option<std::sync::Weak<egui::epaint::mutex::Mutex<egui::epaint::TextureAtlas>>>,
}

impl GalleyRing {
    pub fn bind_atlas(
        &mut self,
        atlas: &Arc<egui::epaint::mutex::Mutex<egui::epaint::TextureAtlas>>,
    ) {
        let current = Arc::downgrade(atlas);
        if self.atlas.as_ref().is_some_and(|old| !old.ptr_eq(&current)) {
            self.entries.clear();
            self.bytes = 0;
        }
        // Prevent allocator address reuse without retaining old atlas pixels.
        self.atlas = Some(current);
    }
    pub fn get(&mut self, key: &GalleyKey) -> Option<Arc<egui::Galley>> {
        let pos = self.entries.iter().position(|e| &e.key == key)?;
        let entry = self.entries.remove(pos)?;
        let galley = entry.galley.clone();
        self.entries.push_back(entry);
        Some(galley)
    }

    pub fn insert(&mut self, key: GalleyKey, galley: Arc<egui::Galley>) -> Arc<egui::Galley> {
        let bytes = galley_weight(&galley);
        if let Some(pos) = self.entries.iter().position(|e| e.key == key) {
            if let Some(old) = self.entries.remove(pos) {
                self.bytes = self.bytes.saturating_sub(old.bytes);
            }
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.entries.push_back(Entry {
            key,
            galley: galley.clone(),
            bytes,
        });
        while self.entries.len() > MAX_ENTRIES || self.bytes > MAX_BYTES {
            if let Some(old) = self.entries.pop_front() {
                self.bytes = self.bytes.saturating_sub(old.bytes);
            } else {
                break;
            }
        }
        galley
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

pub fn galley_ring(ctx: &egui::Context) -> Arc<Mutex<GalleyRing>> {
    let id = egui::Id::new("feather-global-galley-ring-v1");
    ctx.data_mut(|d| {
        d.get_temp::<Arc<Mutex<GalleyRing>>>(id).unwrap_or_else(|| {
            let ring = Arc::new(Mutex::new(GalleyRing::default()));
            d.insert_temp(id, ring.clone());
            ring
        })
    })
}

fn galley_weight(g: &egui::Galley) -> usize {
    // Account for retained vector capacities, not just drawn vertices. Fonts'
    // own frame cache and active output meshes are outside this ring's budget.
    let mut bytes = std::mem::size_of::<egui::Galley>()
        + g.job.text.capacity()
        + g.job.sections.capacity() * std::mem::size_of::<egui::text::LayoutSection>()
        + g.rows.capacity() * std::mem::size_of::<egui::epaint::text::Row>();
    for row in &g.rows {
        bytes = bytes
            .saturating_add(
                row.glyphs.capacity() * std::mem::size_of::<egui::epaint::text::Glyph>(),
            )
            .saturating_add(
                row.visuals.mesh.vertices.capacity() * std::mem::size_of::<egui::epaint::Vertex>(),
            )
            .saturating_add(row.visuals.mesh.indices.capacity() * std::mem::size_of::<u32>());
    }
    bytes.saturating_add(512)
}

/// Bounded cache of parsed inline prose (`LayoutJob`s).
///
/// `prose` re-parses every line of every visible block on every frame, and that
/// parse costs ~5.3 us per line — more than drawing the result. The job depends
/// only on the line text (the formats are fixed by `inline`), so it can be
/// reused across frames. A cache hit is a `LayoutJob::clone` at ~0.5 us, an
/// order of magnitude cheaper.
///
/// Keyed by the *source* line text. The parser consumes the markdown markers,
/// so `LayoutJob::text` is not an identity for an entry — two different sources
/// can produce the same visible text. Each entry therefore stores the source
/// length and is verified by re-fingerprinting the source, exactly the check the
/// caller already performed.
#[derive(Default)]
pub struct JobRing {
    /// `hash -> (hash, source_len, job)`. The hash is stored again so a
    /// collision can be rejected without trusting the map key alone.
    entries: HashMap<u64, (u64, usize, egui::text::LayoutJob)>,
    order: VecDeque<u64>,
}

/// Enough distinct lines for a comfortably long visible conversation, while
/// keeping the retained bytes trivial next to the galley ring's own budget.
pub const MAX_JOB_ENTRIES: usize = 512;

impl JobRing {
    /// Look up a parsed line by its source text.
    ///
    /// `hash` must be a fingerprint of `text` supplied by the caller. The
    /// collision guard compares a *re-derived* fingerprint of `text`, not
    /// `LayoutJob::text`: the parser consumes the markdown markers, so a source
    /// like "**hi**" never equals the "hi" its job carries.
    pub fn get(
        &mut self,
        hash: u64,
        text: &str,
        fingerprint: impl Fn(&str) -> u64,
    ) -> Option<egui::text::LayoutJob> {
        let (stored_hash, len, job) = self.entries.get(&hash)?;
        if *stored_hash != fingerprint(text) || *len != text.len() {
            return None;
        }
        // A hit does not reorder anything: `insert` pushes each key once, so the
        // order queue already holds at most one copy of every live key, and
        // re-appending here would grow it without bound while adding an O(n)
        // scan to every hit. Insertion time therefore defines eviction order —
        // and lines are re-inserted whenever they are parsed again after an
        // eviction, so hot prose does not get stuck at the front.
        Some(job.clone())
    }

    pub fn insert(&mut self, hash: u64, text: &str, job: egui::text::LayoutJob) {
        if self.entries.insert(hash, (hash, text.len(), job)).is_none() {
            self.order.push_back(hash);
        }
        while self.entries.len() > MAX_JOB_ENTRIES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }
}

pub fn job_ring(ctx: &egui::Context) -> Arc<Mutex<JobRing>> {
    let id = egui::Id::new("feather-global-inline-job-ring-v1");
    ctx.data_mut(|d| {
        d.get_temp::<Arc<Mutex<JobRing>>>(id).unwrap_or_else(|| {
            let ring = Arc::new(Mutex::new(JobRing::default()));
            d.insert_temp(id, ring.clone());
            ring
        })
    })
}

#[cfg(test)]
pub fn stats(ctx: &egui::Context) -> (usize, usize) {
    let ring = galley_ring(ctx);
    let ring = ring.lock().unwrap();
    (ring.len(), ring.bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn evicted_galleys_have_no_strong_owner_in_piece_metadata() {
        let ctx = egui::Context::default();
        let mut ring = GalleyRing::default();
        let mut weak = None;
        let _ = ctx.run(Default::default(), |ctx| {
            for i in 0..MAX_ENTRIES + 1 {
                let galley = ctx.fonts(|f| {
                    f.layout(
                        format!("unique {i}"),
                        egui::FontId::proportional(14.0),
                        egui::Color32::WHITE,
                        500.0,
                    )
                });
                if i == 0 {
                    weak = Some(Arc::downgrade(&galley));
                }
                ring.insert(
                    GalleyKey {
                        hash: i as u64,
                        len: galley.text().len(),
                        width: 500,
                        ppp: 1024,
                        atlas: 0,
                        mono: false,
                        size_bits: 14.0f32.to_bits(),
                        color: [255; 4],
                    },
                    galley,
                );
            }
        });
        // egui also owns a one-frame cache; release that independently. The
        // ring itself must hold exactly its cap and no orphan entry handles.
        assert_eq!(ring.len(), MAX_ENTRIES);
        assert!(ring.bytes() <= MAX_BYTES);
        for _ in 0..4 {
            let _ = ctx.run(Default::default(), |_| {});
        }
        assert!(
            weak.unwrap().upgrade().is_none(),
            "evicted layout must actually be reclaimable"
        );
    }
}
