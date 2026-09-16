import io
import struct

TEXT = "#d97757"
FONT = "'Segoe UI Variable Text','Segoe UI',system-ui,-apple-system,Helvetica,Arial,sans-serif"

W, H = 206, 68

svg = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" role="img" aria-label="Lathe">
  <circle cx="26" cy="34" r="17" fill="none" stroke="{TEXT}" stroke-width="9"/>
  <text x="64" y="54" font-family="{FONT}" font-size="58" font-weight="600" fill="{TEXT}" letter-spacing="-1.5">Lathe</text>
</svg>
'''

open("assets/banner.svg", "w", encoding="utf-8").write(svg)
print("wrote banner.svg")

# The social preview: GitHub shows it wherever the repository is linked. 1280x640 is
# GitHub's own recommendation. Uploaded by hand under Settings > Social preview; there
# is no API for it. Needs Pillow and a Segoe font, so it is skipped elsewhere.
try:
    from PIL import Image, ImageDraw, ImageFont
except ImportError:
    print("Pillow not installed; skipping social-preview.png")
    raise SystemExit

PAPER, INK, ASH = "#faf9f5", "#141413", "#7a786f"
PW, PH = 1280, 640
img = Image.new("RGB", (PW, PH), PAPER)
draw = ImageDraw.Draw(img)

def font(size, bold=False):
    for name in (["segoeuib.ttf", "segoeui.ttf"] if bold else ["segoeui.ttf"]):
        try:
            return ImageFont.truetype(f"C:/Windows/Fonts/{name}", size)
        except OSError:
            continue
    return ImageFont.load_default(size)

# The ring from the tray icon, then the wordmark, then one line of what it does.
ring = 34
draw.ellipse((120, 178, 120 + 2 * 68, 178 + 2 * 68), outline=TEXT, width=ring)
draw.text((300, 156), "Lathe", font=font(140, bold=True), fill=TEXT)
draw.text((122, 370), "Local push-to-talk dictation.", font=font(56), fill=INK)
draw.text(
    (122, 452),
    "Hold a key, talk, release. Speech recognition and cleanup run on your own GPU.",
    font=font(30),
    fill=ASH,
)
draw.text((122, 500), "Windows, macOS and Linux. No cloud, no account, no subscription.", font=font(30), fill=ASH)
img.save("assets/social-preview.png", optimize=True)
print("wrote social-preview.png")

# The application icon: the same ring, at every size Windows picks from. The .ico
# used to hold only 256 px, and the title bar and taskbar downscaled it to a blur;
# each size is drawn on its own, supersampled, so 16 px is a ring and not a smudge.
SIZES = (16, 20, 24, 32, 48, 64, 128, 256)

def ring_at(size):
    # Radius and stroke are whole pixels, so the ring's edges land on pixel boundaries
    # and only the diagonals are anti-aliased; a fractional stroke smears across two
    # half-covered pixels and reads as blur at taskbar size. Below 24 px a proportional
    # stroke thins to a hairline, so it is held at two pixels.
    radius = round(size * 0.40)
    width = max(round(size * 0.11), 2)
    ss = 8
    big = size * ss
    im = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    c = big / 2
    r = radius * ss
    d.ellipse((c - r, c - r, c + r, c + r), outline=TEXT, width=width * ss)
    return im.resize((size, size), Image.BOX)

icons = {size: ring_at(size) for size in SIZES}
for size in (32, 128, 256):
    icons[size].save(f"crates/lathe/icons/{size}x{size}.png", optimize=True)

# Written by hand rather than through Pillow's ICO writer, which PNG-compresses every
# entry. The shell only decodes PNG reliably for the 256 px entry; taskbar-sized ones
# stored that way come out pixelated. Everything smaller is a plain 32-bit bitmap.
def bmp_entry(im):
    w, h = im.size
    rows = b"".join(
        bytes(b for px in (im.getpixel((x, y)) for x in range(w)) for b in (px[2], px[1], px[0], px[3]))
        for y in reversed(range(h))
    )
    mask = b"\0" * (((w + 31) // 32) * 4 * h)
    header = struct.pack("<IiiHHIIiiII", 40, w, h * 2, 1, 32, 0, len(rows) + len(mask), 0, 0, 0, 0)
    return header + rows + mask

def png_entry(im):
    buf = io.BytesIO()
    im.save(buf, "PNG", optimize=True)
    return buf.getvalue()

entries = [(s, png_entry(icons[s]) if s == 256 else bmp_entry(icons[s])) for s in SIZES]
offset = 6 + 16 * len(entries)
directory = b""
for s, data in entries:
    directory += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(data), offset)
    offset += len(data)
with open("crates/lathe/icons/icon.ico", "wb") as f:
    f.write(struct.pack("<HHH", 0, 1, len(entries)) + directory + b"".join(d for _, d in entries))
print("wrote crates/lathe/icons")
