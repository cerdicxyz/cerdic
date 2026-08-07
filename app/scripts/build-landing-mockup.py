"""
Builds iphone-mockup-composited.png from iphone-mockup-v2.png (raw
studio-photo mockup) + iphone-12-50-p.png (flat mobile screenshot).

Consolidates everything learned iterating on this by hand:
1. Background removal: iphone-mockup-v2.png has a fully opaque light-gray
   studio-gradient background, not a flat color, so a plain flood-fill
   leaks straight through it. Classify by per-pixel brightness instead
   (phone is dark <148, background is bright), then keep only the
   LARGEST connected component of that classification — this throws out
   the soft drop-shadow blob under the phone (which also falls under the
   threshold but isn't touching the phone) without needing to reason
   about its shape.
2. Sparkle removal: the source photo has a small decorative sparkle/glint
   graphic overlapping the bottom-right corner. Part of it is dark enough
   to sneak under the brightness threshold and fuse onto the phone's
   silhouette via the closing step below. Detected and cut by local
   brightness within a small window around its known position, not a
   global rule (a global brighter-highlight cutoff would also remove the
   legitimate metal-edge reflection elsewhere on the bezel).
3. Morphological closing+opening on the kept silhouette to bridge small
   gaps in the thin outer highlight ring (real reflections that dip in
   and out of the threshold) without changing the overall shape.
4. Bottom-right corner mirror: even after 1-3, the bottom-right corner's
   own bezel/highlight geometry in the source photo doesn't fully match
   the smooth curve everywhere (residual sub-artifacts survived several
   rounds of threshold/sparkle tuning). Rather than keep chasing that by
   eye, the bottom-LEFT corner (verified clean) is mirrored horizontally
   on top of the bottom-right, feathered over a wide transition band so
   the swap is invisible — this makes the corner correct BY
   CONSTRUCTION, independent of whatever is actually in the source pixels
   there.
5. Screen-hole cut: flood-fill from the image center on
   alpha<40, |color - center_color| <= TOL to cut the screen transparent.
6. Tight crop to the opaque bbox.
7. Compositing: the screenshot is pasted onto a BLACK canvas (not the
   frame's rectangular hole bbox) then clipped to the frame's actual
   flood-filled hole SHAPE (not its bounding rectangle) before the frame
   is alpha-composited on top. Using the bbox rectangle directly let its
   square corners poke past the frame's own rounded corners. A small
   margin inset keeps real screenshot content (icons, stats) clear of the
   notch/corner zone, since the screenshot was designed for a plain
   rectangular viewport with near-zero edge padding of its own.
"""

from collections import deque

import numpy as np
from PIL import Image
from scipy import ndimage

LANDING_DIR = "../public/landing"
SOURCE_FRAME = f"{LANDING_DIR}/iphone-mockup-v2.png"
SOURCE_SHOT = f"{LANDING_DIR}/iphone-12-50-p.png"
OUTPUT = f"{LANDING_DIR}/iphone-mockup-composited.png"

BRIGHTNESS_CUTOFF = 148  # valley between the phone-dark and bg-bright histogram clusters
CLOSE_KERNEL = 9
OPEN_KERNEL = 3
SPARKLE_WINDOW_FRAC = (0.82, 0.91, 0.77, 0.90)  # (y0,y1,x0,x1) as fraction of (h,w)
SPARKLE_BRIGHTNESS = 150
SCREEN_HOLE_TOL = 35
CROP_PAD = 4
MARGIN_PCT = 0.035  # inset for real screenshot content, clear of notch/corners
CORNER_MIRROR_PATCH = 340  # square region size, big enough to cover the full corner curve
CORNER_MIRROR_FEATHER = 90  # transition width blending the mirrored patch into the real photo


def remove_background_and_sparkle(path: str) -> Image.Image:
    im = Image.open(path).convert("RGBA")
    arr = np.array(im)
    h, w = arr.shape[:2]
    brightness = arr[:, :, :3].astype(float).mean(axis=2)

    opaque = brightness < BRIGHTNESS_CUTOFF
    labeled, n = ndimage.label(opaque)
    sizes = ndimage.sum(opaque, labeled, range(1, n + 1))
    phone_mask = labeled == (np.argmax(sizes) + 1)
    phone_mask = ndimage.binary_closing(phone_mask, structure=np.ones((CLOSE_KERNEL, CLOSE_KERNEL)))
    phone_mask = ndimage.binary_opening(phone_mask, structure=np.ones((OPEN_KERNEL, OPEN_KERNEL)))

    yf0, yf1, xf0, xf1 = SPARKLE_WINDOW_FRAC
    y0, y1, x0, x1 = int(h * yf0), int(h * yf1), int(w * xf0), int(w * xf1)
    local_bright = brightness[y0:y1, x0:x1] > SPARKLE_BRIGHTNESS
    local_labeled, local_n = ndimage.label(local_bright)
    if local_n > 0:
        local_sizes = ndimage.sum(local_bright, local_labeled, range(1, local_n + 1))
        sparkle_blob = local_labeled == (np.argmax(local_sizes) + 1)
        region = phone_mask[y0:y1, x0:x1]
        region[sparkle_blob] = False
        phone_mask[y0:y1, x0:x1] = region
        # Reseal whatever thin gap that removal left in the bezel ring.
        phone_mask = ndimage.binary_closing(phone_mask, structure=np.ones((5, 5)))

    out = arr.copy()
    out[:, :, 3] = (phone_mask * 255).astype(np.uint8)
    return Image.fromarray(out)


def flood_fill_from_center(arr: np.ndarray, alpha_max: int | None, tol: int | None) -> np.ndarray:
    """Generic BFS flood fill from image center. If tol is given, matches
    on color similarity to the center pixel (used for the screen hole);
    otherwise matches on alpha < alpha_max (used for the frame's hole
    shape at composite time)."""
    h, w = arr.shape[:2]
    alpha = arr[:, :, 3]
    cy, cx = h // 2, w // 2
    mask = np.zeros((h, w), dtype=bool)
    visited = np.zeros((h, w), dtype=bool)
    q = deque([(cy, cx)])
    visited[cy, cx] = True
    rgb = arr[:, :, :3].astype(int)
    target = rgb[cy, cx]
    while q:
        y, x = q.popleft()
        mask[y, x] = True
        for dy, dx in ((1, 0), (-1, 0), (0, 1), (0, -1)):
            ny, nx = y + dy, x + dx
            if 0 <= ny < h and 0 <= nx < w and not visited[ny, nx]:
                visited[ny, nx] = True
                if tol is not None:
                    c = rgb[ny, nx]
                    match = (
                        alpha[ny, nx] > 200
                        and abs(c[0] - target[0]) <= tol
                        and abs(c[1] - target[1]) <= tol
                        and abs(c[2] - target[2]) <= tol
                    )
                else:
                    match = alpha[ny, nx] < alpha_max
                if match:
                    mask[ny, nx] = True
                    q.append((ny, nx))
    return mask


def mirror_bottom_right_corner(im: Image.Image) -> Image.Image:
    """Replaces the bottom-right corner with a horizontally-mirrored copy
    of the bottom-left corner (verified clean), feathered in over a wide
    band so the swap is invisible. Makes that corner correct by
    construction instead of depending on precisely identifying every
    artifact in the source photo there."""
    arr = np.array(im).astype(np.float64)
    h, w = arr.shape[:2]
    patch, feather = CORNER_MIRROR_PATCH, CORNER_MIRROR_FEATHER

    bl = arr[h - patch : h, 0:patch]
    bl_mirrored = bl[:, ::-1]

    yy, xx = np.mgrid[0:patch, 0:patch]
    dist_from_top = yy
    dist_from_right_side_of_patch = patch - 1 - xx
    edge_dist = np.minimum(dist_from_top, dist_from_right_side_of_patch)
    weight = np.clip(edge_dist / feather, 0, 1)[:, :, None]

    region = arr[h - patch : h, w - patch : w]
    arr[h - patch : h, w - patch : w] = bl_mirrored * weight + region * (1 - weight)
    return Image.fromarray(arr.astype(np.uint8))


def cut_screen_hole(im: Image.Image) -> Image.Image:
    arr = np.array(im)
    mask = flood_fill_from_center(arr, alpha_max=None, tol=SCREEN_HOLE_TOL)
    arr[mask, 3] = 0
    return Image.fromarray(arr)


def crop_to_bbox(im: Image.Image) -> Image.Image:
    arr = np.array(im)
    alpha = arr[:, :, 3]
    ys, xs = np.where(alpha > 200)
    x0, x1, y0, y1 = xs.min(), xs.max(), ys.min(), ys.max()
    h, w = arr.shape[:2]
    cx0, cy0 = max(0, x0 - CROP_PAD), max(0, y0 - CROP_PAD)
    cx1, cy1 = min(w, x1 + CROP_PAD), min(h, y1 + CROP_PAD)
    return im.crop((cx0, cy0, cx1, cy1))


def composite_screenshot(frame: Image.Image, shot: Image.Image) -> Image.Image:
    farr = np.array(frame)
    fh, fw = farr.shape[:2]
    hole_mask = flood_fill_from_center(farr, alpha_max=40, tol=None)

    ys, xs = np.where(hole_mask)
    hx0, hx1, hy0, hy1 = xs.min(), xs.max(), ys.min(), ys.max()
    hole_w, hole_h = hx1 - hx0, hy1 - hy0

    mx, my = round(hole_w * MARGIN_PCT), round(hole_h * MARGIN_PCT)
    inner_x0, inner_y0 = hx0 + mx, hy0 + my
    inner_w, inner_h = (hx1 - mx) - inner_x0, (hy1 - my) - inner_y0

    shot_w, shot_h = shot.size
    scale = max(inner_w / shot_w, inner_h / shot_h)
    resized = shot.resize((round(shot_w * scale), round(shot_h * scale)), Image.LANCZOS)
    rw, rh = resized.size
    crop_x, crop_y = (rw - inner_w) // 2, (rh - inner_h) // 2
    shot_cropped = resized.crop((crop_x, crop_y, crop_x + inner_w, crop_y + inner_h))

    screen_layer = Image.new("RGBA", (fw, fh), (0, 0, 0, 255))
    screen_layer.paste(shot_cropped, (inner_x0, inner_y0))
    screen_arr = np.array(screen_layer)
    screen_arr[~hole_mask, 3] = 0  # clip to the real hole SHAPE, not its bbox rectangle
    screen_layer = Image.fromarray(screen_arr)

    return Image.alpha_composite(screen_layer, frame)


def main():
    frame = remove_background_and_sparkle(SOURCE_FRAME)
    frame = crop_to_bbox(frame)
    # Mirror BEFORE cutting the screen hole, not after: cutting first and
    # mirroring second blends a feathered, partially-transparent edge
    # into what should be one clean hard boundary, which let a thin
    # sliver of the screenshot bleed through right at the seam. Mirroring
    # the still-opaque screen first means the flood-fill below re-derives
    # a single consistent hole edge across the whole image, mirrored
    # region included.
    frame = mirror_bottom_right_corner(frame)
    frame = cut_screen_hole(frame)

    shot = Image.open(SOURCE_SHOT).convert("RGBA")
    composited = composite_screenshot(frame, shot)
    # No bottom shave: that was working around the bottom-right corner
    # artifact by hiding it, which mirror_bottom_right_corner now fixes
    # for real — the full phone (rounded corner and all) renders clean.

    composited.save(OUTPUT)
    print("saved", OUTPUT, composited.size)


if __name__ == "__main__":
    main()
