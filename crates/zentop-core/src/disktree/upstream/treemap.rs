// Copyright (c) 2026 Tobi Lütke
// SPDX-License-Identifier: MIT
// Copied from tobi/disktree at 9b18b272ca3c5a145f6ff58eaf89dc4d311eeaea:
// crates/disktree-core/src/treemap.rs
// License: ../../../../../licenses/disktree-MIT.md (repository root).
// Component extraction/adaptation details: UPSTREAM.md in this directory.

//! Upstream squarification geometry; independent of the upstream tree model.

/// An axis-aligned rectangle in viewport pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn area(&self) -> f32 {
        (self.w.max(0.0)) * (self.h.max(0.0))
    }
}

/// Split `area` into one rectangle per value, proportional to it.
///
/// Returned in the order of `values`, not in layout order.
pub fn squarify(values: &[f64], area: Rect) -> Vec<Rect> {
    let mut rects = vec![Rect::default(); values.len()];
    let total: f64 = values.iter().filter(|value| **value > 0.0).sum();
    if total <= 0.0 || area.w <= 0.0 || area.h <= 0.0 {
        return rects;
    }

    let mut order: Vec<usize> = (0..values.len()).filter(|i| values[*i] > 0.0).collect();
    order.sort_by(|left, right| values[*right].total_cmp(&values[*left]));

    let scale = f64::from(area.w) * f64::from(area.h) / total;
    let areas: Vec<f64> = order.iter().map(|index| values[*index] * scale).collect();

    let mut free = area;
    let mut start = 0;
    while start < areas.len() {
        let side = f64::from(free.w.min(free.h));
        let mut end = start + 1;
        let mut row_sum = areas[start];
        let mut row_worst = worst_ratio(&areas[start..end], row_sum, side);

        while end < areas.len() {
            let candidate_sum = row_sum + areas[end];
            let candidate_worst = worst_ratio(&areas[start..=end], candidate_sum, side);
            if candidate_worst > row_worst {
                break;
            }
            row_sum = candidate_sum;
            row_worst = candidate_worst;
            end += 1;
        }

        if free.w >= free.h {
            // A vertical strip on the left; tiles stack top to bottom.
            let strip_w = (row_sum / f64::from(free.h)) as f32;
            let strip_w = strip_w.min(free.w);
            let mut y = free.y;
            for index in start..end {
                let height = if strip_w > 0.0 {
                    (areas[index] / f64::from(strip_w)) as f32
                } else {
                    0.0
                };
                let height = height.min(free.bottom() - y).max(0.0);
                rects[order[index]] = Rect::new(free.x, y, strip_w, height);
                y += height;
            }
            free.x += strip_w;
            free.w -= strip_w;
        } else {
            // A horizontal strip along the top; tiles run left to right.
            let strip_h = (row_sum / f64::from(free.w)) as f32;
            let strip_h = strip_h.min(free.h);
            let mut x = free.x;
            for index in start..end {
                let width = if strip_h > 0.0 {
                    (areas[index] / f64::from(strip_h)) as f32
                } else {
                    0.0
                };
                let width = width.min(free.right() - x).max(0.0);
                rects[order[index]] = Rect::new(x, free.y, width, strip_h);
                x += width;
            }
            free.y += strip_h;
            free.h -= strip_h;
        }
        start = end;
    }
    rects
}

/// Worst (largest) aspect ratio in a row of `areas` laid along `side`.
fn worst_ratio(areas: &[f64], row_sum: f64, side: f64) -> f64 {
    if row_sum <= 0.0 || side <= 0.0 {
        return f64::INFINITY;
    }
    let thickness = row_sum / side;
    areas.iter().fold(0.0_f64, |worst, area| {
        if *area <= 0.0 || thickness <= 0.0 {
            return worst;
        }
        let other = area / thickness;
        let ratio = (thickness / other).max(other / thickness);
        worst.max(ratio)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn area() -> Rect {
        Rect::new(0.0, 0.0, 800.0, 500.0)
    }
    #[test]
    fn squarify_fills_the_area() {
        let values = [40.0, 30.0, 20.0, 5.0, 3.0, 2.0];
        let rects = squarify(&values, area());
        let covered: f32 = rects.iter().map(Rect::area).sum();
        assert!(
            (covered - area().area()).abs() < 1.0,
            "covered {covered} of {}",
            area().area()
        );
        for rect in &rects {
            assert!(rect.x >= -0.01 && rect.y >= -0.01);
            assert!(rect.right() <= area().right() + 0.01);
            assert!(rect.bottom() <= area().bottom() + 0.01);
        }
    }

    #[test]
    fn squarify_keeps_aspect_ratios_reasonable() {
        // Same distribution as the canonical squarify example: 6, 6, 4, 3, 2, 2,
        // 1 on a 6x4 canvas.
        let values = [6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0];
        let rects = squarify(&values, Rect::new(0.0, 0.0, 600.0, 400.0));
        for rect in &rects {
            let ratio = (rect.w / rect.h).max(rect.h / rect.w);
            assert!(ratio <= 4.0, "aspect ratio {ratio} in {rect:?}");
        }
    }

    #[test]
    fn squarify_survives_degenerate_input() {
        assert_eq!(squarify(&[], area()).len(), 0);
        assert!(squarify(&[0.0, 0.0], area())
            .iter()
            .all(|r| r.area().abs() < f32::EPSILON));
        assert!(
            squarify(&[1.0], Rect::new(0.0, 0.0, 0.0, 10.0))[0]
                .area()
                .abs()
                < f32::EPSILON
        );
    }
}
