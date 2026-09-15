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
