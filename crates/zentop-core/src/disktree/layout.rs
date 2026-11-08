use super::View;
#[derive(Clone, Copy, Debug, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}
impl Rect {
    pub fn contains(self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Tile {
    pub id: usize,
    pub rect: Rect,
    pub depth: usize,
}

/// Squarified, area-proportional rectangles; parents reserve a label row.
/// Rendering is bounded independently of how many filesystem entries were scanned.
pub fn layout(view: &View, width: f64, height: f64, header: f64, gap: f64) -> Vec<Tile> {
    if !width.is_finite() || !height.is_finite() || width <= 0. || height <= 0. {
        return vec![];
    }
    let mut tiles = Vec::new();
    let mut stack = vec![(
        view.zoom,
        Rect {
            x: 0.,
            y: 0.,
            w: width,
            h: height,
        },
        0,
    )];
    while let Some((parent, rect, depth)) = stack.pop() {
        if depth >= view.options.depth.clamp(1, 8) || tiles.len() >= 5000 {
            continue;
        }
        let mut children: Vec<_> = view.tree.nodes[parent]
            .children
            .iter()
            .copied()
            .filter(|i| view.weights[*i] > 0.)
            .collect();
        children.sort_by(|a, b| view.weights[*b].total_cmp(&view.weights[*a]));
        let total: f64 = children.iter().map(|i| view.weights[*i]).sum();
        if total <= 0. {
            continue;
        }
        let values: Vec<_> = children.iter().map(|id| view.weights[*id]).collect();
        let area = super::upstream::treemap::Rect::new(
            rect.x as f32,
            rect.y as f32,
            rect.w as f32,
            rect.h as f32,
        );
        if area.area() <= 0. {
            continue;
        }
        let placed: Vec<_> = children
            .into_iter()
            .zip(super::upstream::treemap::squarify(&values, area))
            .map(|(id, r)| {
                (
                    id,
                    Rect {
                        x: f64::from(r.x),
                        y: f64::from(r.y),
                        w: f64::from(r.w),
                        h: f64::from(r.h),
                    },
                )
            })
            .collect();
        for (id, rect) in placed {
            if rect.w < 1. || rect.h < 1. || tiles.len() >= 5000 {
                continue;
            }
            tiles.push(Tile { id, rect, depth });
            let inner = Rect {
                x: rect.x + gap,
                y: rect.y + header,
                w: (rect.w - 2. * gap).max(0.),
                h: (rect.h - header - gap).max(0.),
            };
            if inner.w >= 2. && inner.h >= 2. {
                stack.push((id, inner, depth + 1));
            }
        }
    }
    // Parents must paint before children; deepest rectangles win hit testing.
    tiles.sort_by_key(|t| t.depth);
    tiles
}
