"""Rueca v13: the R mark (from v12) — rueca + Rust.

stem   = solid upright of the R
bowl   = the spinning wheel
leg    = the bobbin, curly braces { } with the red thread wound between,
         leaning on the stem where an R's leg starts

v13 vs v12: brace no longer collides with the rim, leg anchored to the stem,
softer coil (fewer, rounded turns), two stroke weights + spokes, mark scaled
to fit GitHub's circular avatar crop, small-size glyph (<= 48 px), on-dark
transparent variant and a 1280x640 social card.
"""
import math
import sys

f = lambda p: f"{p[0]:.1f} {p[1]:.1f}"
P = lambda c, r, t: (c[0] + r * math.cos(t), c[1] + r * math.sin(t))
CAP = 'stroke-linecap="round" stroke-linejoin="round"'

LINE = 24          # rim, braces
SPOKE = 14
THREAD = 13
STEM_W = 58


def geometry():
    g = {}
    g["stem_x0"], g["top"], g["base"] = 92, 66, 452
    g["stem_x1"] = g["stem_x0"] + STEM_W
    g["R"] = 92
    g["C"] = (g["stem_x1"] + LINE / 2 + g["R"] - 8, g["top"] + LINE / 2 + g["R"])
    C, R = g["C"], g["R"]
    # leg: starts against the stem, below the bowl, runs to the bottom right
    g["p0"] = (g["stem_x1"] + 34, C[1] + R + 74)   # brace touches the stem, clears the rim by ~13 px
    g["p1"] = (376, g["base"])
    return g


def mark(ink, red):
    g = geometry()
    s = []
    C, R = g["C"], g["R"]
    s.append(f'<rect x="{g["stem_x0"]}" y="{g["top"]}" width="{STEM_W}" height="{g["base"] - g["top"]}" rx="10" fill="{ink}"/>')
    s.append(f'<circle cx="{C[0]:.1f}" cy="{C[1]:.1f}" r="{R}" stroke="{ink}" stroke-width="{LINE}"/>')
    for i in range(6):
        t = math.radians(60 * i + 30)
        a, b = P(C, 24, t), P(C, R, t)
        s.append(f'<path d="M {f(a)} L {f(b)}" stroke="{ink}" stroke-width="{SPOKE}" stroke-linecap="round"/>')
    s.append(f'<circle cx="{C[0]:.1f}" cy="{C[1]:.1f}" r="24" fill="{ink}"/>')

    p0, p1 = g["p0"], g["p1"]
    L = math.hypot(p1[0] - p0[0], p1[1] - p0[1])
    ang = math.degrees(math.atan2(p1[1] - p0[1], p1[0] - p0[0]))
    h = 36
    k = 2 * h + 24
    parts = []

    def brace(u, flip):
        pts = [(.75, 0), (.45, 0), (.5, .12), (.5, .3), (.5, .44), (.42, .5), (.22, .5), (.42, .5), (.5, .56), (.5, .7), (.5, .88), (.45, 1), (.75, 1)]
        q = [(u + flip * (x - .5) * k * .8, (y - .5) * k) for x, y in pts]
        d = f"M {f(q[0])}" + "".join(f" C {f(q[i])} {f(q[i + 1])} {f(q[i + 2])}" for i in range(1, 13, 3))
        return f'<path d="{d}" stroke="{ink}" stroke-width="{LINE}" {CAP}/>'

    parts.append(brace(0, 1))
    parts.append(brace(L, -1))
    # wound thread: parallel turns across the bobbin, like yarn on a spool
    u0, u1, hv = 30, L - 30, h - 6
    n = 7
    step = (u1 - u0) / (n - 1)
    d = ""
    for i in range(n):
        u = u0 + step * i
        d += f" M {u - 7:.1f} {-hv} L {u + 7:.1f} {hv}"
    parts.append(f'<path d="{d.strip()}" stroke="{red}" stroke-width="{THREAD}" {CAP}/>')
    s.append(f'<g transform="translate({p0[0]:.1f} {p0[1]:.1f}) rotate({ang:.2f})">{"".join(parts)}</g>')

    # thread leaves the rim and drops into the first turn
    ca, sa = math.cos(math.radians(ang)), math.sin(math.radians(ang))
    start = (p0[0] + (u0 - 7) * ca + hv * sa, p0[1] + (u0 - 7) * sa - hv * ca)
    out = P(C, R + LINE / 2 - 2, math.radians(50))
    s.append(f'<path d="M {f(out)} C {f((out[0] + 8, out[1] + 34))} {f((start[0] + 30, start[1] - 22))} {f(start)}" stroke="{red}" stroke-width="{THREAD}" {CAP}/>')
    return "".join(s)


def glyph(ink, red):
    """Small-size mark (<= 48 px): stem, wheel with hub + 3 spokes, solid leg."""
    g = geometry()
    C, R = g["C"], g["R"]
    s = [f'<rect x="{g["stem_x0"] - 8}" y="{g["top"]}" width="{STEM_W + 16}" height="{g["base"] - g["top"]}" rx="14" fill="{ink}"/>',
         f'<circle cx="{C[0]:.1f}" cy="{C[1]:.1f}" r="{R}" stroke="{ink}" stroke-width="40"/>',
         f'<circle cx="{C[0]:.1f}" cy="{C[1]:.1f}" r="30" fill="{ink}"/>']
    for i in range(3):
        t = math.radians(120 * i + 90)
        s.append(f'<path d="M {f(C)} L {f(P(C, R, t))}" stroke="{ink}" stroke-width="24" stroke-linecap="round"/>')
    p0 = (g["stem_x1"] + 30, C[1] + R + 40)
    s.append(f'<path d="M {f(p0)} L {f(g["p1"])}" stroke="{red}" stroke-width="64" stroke-linecap="round"/>')
    return "".join(s)


def svg(body, bg=None, w=512, h=512, inner=None):
    b = f'<rect width="{w}" height="{h}" fill="{bg}"/>' if bg else ""
    inner = inner or '<g transform="translate(256 256) scale(0.9) translate(-248 -259)">{}</g>'
    return f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" fill="none">{b}{inner.format(body)}</svg>'


INK_L, RED_L, BG_L = "#16181D", "#E8451F", "#FFFFFF"
INK_D, RED_D, BG_D = "#ECEEF2", "#FF5A2E", "#0D1117"
FONT = "ui-sans-serif, -apple-system, 'Segoe UI', Inter, Helvetica, Arial, sans-serif"


def social(ink, red, bg, sub):
    body = mark(ink, red)
    text = (f'<text x="560" y="330" font-family="{FONT}" font-size="168" font-weight="700" fill="{ink}" letter-spacing="-4">rueca</text>'
            f'<text x="566" y="404" font-family="{FONT}" font-size="38" fill="{sub}">code intelligence for coding agents</text>')
    inner = '<g transform="translate(90 64)">{}</g>' + text
    return svg(body, bg, 1280, 640, inner)


if __name__ == "__main__":
    out = sys.argv[1]
    files = {
        "-light.svg": svg(mark(INK_L, RED_L), BG_L),
        "-dark.svg": svg(mark(INK_D, RED_D), BG_D),
        ".svg": svg(mark(INK_L, RED_L)),
        "-on-dark.svg": svg(mark(INK_D, RED_D)),
        "-small.svg": svg(glyph(INK_L, RED_L)),
        "-small-on-dark.svg": svg(glyph(INK_D, RED_D)),
        "-small-light.svg": svg(glyph(INK_L, RED_L), BG_L),
        "-social-light.svg": social(INK_L, RED_L, BG_L, "#57606A"),
        "-social-dark.svg": social(INK_D, RED_D, BG_D, "#9198A1"),
    }
    for suffix, content in files.items():
        open(out + suffix, "w").write(content)
