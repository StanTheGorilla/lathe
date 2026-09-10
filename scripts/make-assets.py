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
