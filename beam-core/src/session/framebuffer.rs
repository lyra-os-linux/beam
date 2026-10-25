//! Shared BGRA framebuffer for one session.
//!
//! The active session task owns the decode side (writing pixels as PDUs arrive) while the
//! frontend reads snapshots to paint. Reads and writes are decoupled from event delivery: the
//! event channel only carries *notifications* that pixels changed ([`crate::events::SessionEvent::FramebufferUpdated`]),
//! never the pixel data itself, so a slow/backpressured UI thread never blocks the network task
//! and large frames are never cloned through the channel just to report a heartbeat.

use std::sync::Mutex;

/// An axis-aligned region of the framebuffer that changed in a single update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

struct Inner {
    width: u16,
    height: u16,
    generation: u64,
    /// Tightly packed BGRA8888 pixels, `height` rows of `width * 4` bytes.
    data: Vec<u8>,
}

/// Thread-safe holder for the current desktop image of a session.
pub struct Framebuffer {
    inner: Mutex<Inner>,
}

impl Framebuffer {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            inner: Mutex::new(Inner {
                width,
                height,
                generation: 0,
                data: vec![0; usize::from(width) * usize::from(height) * 4],
            }),
        }
    }

    /// Replace the buffer contents wholesale (used after a resize / reactivation).
    pub(crate) fn replace(&self, width: u16, height: u16, data: Vec<u8>) {
        let mut inner = self.inner.lock().expect("framebuffer mutex poisoned");
        inner.width = width;
        inner.height = height;
        inner.data = data;
        inner.generation = inner.generation.wrapping_add(1);
    }

    /// Copy a fresh region from `src` (the session's live `DecodedImage` buffer, same
    /// dimensions) into our shared buffer.
    /// Copy only `rect` from a same-sized BGRA source image.
    pub fn update_region(&self, src: &[u8], width: u16, height: u16, rect: DirtyRect) {
        let mut inner = self.inner.lock().expect("framebuffer mutex poisoned");
        if inner.width != width || inner.height != height {
            inner.width = width;
            inner.height = height;
            inner.data.resize(src.len(), 0);
        }
        let stride = usize::from(width) * 4;
        let left = usize::from(rect.x) * 4;
        let right = (usize::from(rect.x) + usize::from(rect.width)).min(usize::from(width)) * 4;
        let bottom = (usize::from(rect.y) + usize::from(rect.height)).min(usize::from(height));
        for row in usize::from(rect.y)..bottom {
            let start = row * stride + left;
            let end = row * stride + right;
            if end <= src.len() && end <= inner.data.len() {
                inner.data[start..end].copy_from_slice(&src[start..end]);
            }
        }
        inner.generation = inner.generation.wrapping_add(1);
    }

    /// Take a snapshot of the current buffer for rendering. Returns `(width, height, bgra_bytes)`.
    pub fn snapshot(&self) -> (u16, u16, Vec<u8>) {
        let inner = self.inner.lock().expect("framebuffer mutex poisoned");
        (inner.width, inner.height, inner.data.clone())
    }

    /// Copy `rect` (clipped to the buffer) as tightly packed RGBA, the layout a web canvas
    /// expects. Returns the clipped rectangle with its pixels, or `None` when it is empty.
    pub fn copy_region_rgba(&self, rect: DirtyRect) -> Option<(DirtyRect, Vec<u8>)> {
        let inner = self.inner.lock().expect("framebuffer mutex poisoned");
        let right = (u32::from(rect.x) + u32::from(rect.width)).min(u32::from(inner.width));
        let bottom = (u32::from(rect.y) + u32::from(rect.height)).min(u32::from(inner.height));
        if right <= u32::from(rect.x) || bottom <= u32::from(rect.y) {
            return None;
        }
        let clipped = DirtyRect {
            x: rect.x,
            y: rect.y,
            width: (right - u32::from(rect.x)) as u16,
            height: (bottom - u32::from(rect.y)) as u16,
        };
        let stride = usize::from(inner.width) * 4;
        let mut out =
            Vec::with_capacity(usize::from(clipped.width) * usize::from(clipped.height) * 4);
        for row in usize::from(clipped.y)..bottom as usize {
            let start = row * stride + usize::from(clipped.x) * 4;
            let end = start + usize::from(clipped.width) * 4;
            for offset in (start..end).step_by(4) {
                let pixel = &inner.data[offset..offset + 4];
                out.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
            }
        }
        Some((clipped, out))
    }

    /// Return cheap render metadata, allowing frontends to reuse an existing texture.
    pub fn metadata(&self) -> (u16, u16, u64) {
        let inner = self.inner.lock().expect("framebuffer mutex poisoned");
        (inner.width, inner.height, inner.generation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_region_rgba_swaps_channels_and_clips_to_the_buffer() {
        let fb = Framebuffer::new(3, 2);
        // BGRA source: pixel (x, y) = [b=x, g=y, r=9, a=0].
        let mut src = Vec::new();
        for y in 0..2u8 {
            for x in 0..3u8 {
                src.extend_from_slice(&[x, y, 9, 0]);
            }
        }
        fb.update_region(
            &src,
            3,
            2,
            DirtyRect {
                x: 0,
                y: 0,
                width: 3,
                height: 2,
            },
        );
        let (rect, rgba) = fb
            .copy_region_rgba(DirtyRect {
                x: 1,
                y: 1,
                width: 10,
                height: 10,
            })
            .unwrap();
        assert_eq!(
            rect,
            DirtyRect {
                x: 1,
                y: 1,
                width: 2,
                height: 1
            }
        );
        assert_eq!(rgba, vec![9, 1, 1, 255, 9, 1, 2, 255]);
        assert!(fb
            .copy_region_rgba(DirtyRect {
                x: 3,
                y: 0,
                width: 1,
                height: 1
            })
            .is_none());
    }

    #[test]
    fn update_region_only_copies_dirty_pixels() {
        let framebuffer = Framebuffer::new(3, 2);
        let source = vec![7; 3 * 2 * 4];
        framebuffer.update_region(
            &source,
            3,
            2,
            DirtyRect {
                x: 1,
                y: 0,
                width: 1,
                height: 2,
            },
        );
        let (_, _, data) = framebuffer.snapshot();
        assert_eq!(&data[4..8], &[7; 4]);
        assert_eq!(&data[16..20], &[7; 4]);
        assert!(data[..4].iter().all(|byte| *byte == 0));
        assert!(data[8..12].iter().all(|byte| *byte == 0));
    }
}
