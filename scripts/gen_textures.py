#!/usr/bin/env python3
"""gen_textures.py — процедурные ТАЙЛИЩИЕСЯ текстуры для демо «Остров» (этап 9).

Всё детерминировано (seed фиксирован), лицензия — та же, что у кода проекта:
текстуры генерируются этим скриптом, внешних ассетов нет.

Периодический value-noise: решётка со случаем значением в узлах, билинейная
интерполяция с ОКАТЫВАНИЕМ индексов (np.roll-семантика) → текстура бесшовно
тайлится. fbm = сумма октав.

Запуск: python3 scripts/gen_textures.py   (создаёт assets/textures/*.png)
"""
import numpy as np
from PIL import Image
import os

OUT = os.path.join(os.path.dirname(__file__), "..", "assets", "textures")
os.makedirs(OUT, exist_ok=True)
rng = np.random.default_rng(20261110)


def lattice(n, freq, seed):
    """Решётка freq×freq случайных значений → билинейно развёрнутая на n×n
    с заворачиванием (тайлится)."""
    r = np.random.default_rng(seed)
    g = r.random((freq, freq))
    ys = np.linspace(0, freq, n, endpoint=False)
    xs = np.linspace(0, freq, n, endpoint=False)
    y0 = np.floor(ys).astype(int)
    x0 = np.floor(xs).astype(int)
    ty = (ys - y0)[:, None]
    tx = (xs - x0)[None, :]
    y1 = (y0 + 1) % freq
    x1 = (x0 + 1) % freq
    a = g[np.ix_(y0, x0)]
    b = g[np.ix_(y0, x1)]
    c = g[np.ix_(y1, x0)]
    d = g[np.ix_(y1, x1)]
    # smoothstep
    sy = ty * ty * (3 - 2 * ty)
    sx = tx * tx * (3 - 2 * tx)
    top = a * (1 - sx) + b * sx
    bot = c * (1 - sx) + d * sx
    return top * (1 - sy) + bot * sy


def fbm(n, base_freq, octaves, seed, gain=0.5):
    acc = np.zeros((n, n))
    amp, f, s = 1.0, base_freq, seed
    total = 0.0
    for _ in range(octaves):
        acc += amp * lattice(n, f, s)
        total += amp
        amp *= gain
        f *= 2
        s += 101
    return acc / total


def save(name, rgb, alpha=None):
    if alpha is None:
        img = Image.fromarray((np.clip(rgb, 0, 1) * 255).astype(np.uint8), "RGB")
    else:
        # премультипликация: у прозрачных текселей RGB→0, иначе белые ореолы
        a = np.clip(alpha, 0, 1)[..., None]
        rgba = np.dstack([np.clip(rgb, 0, 1) * a, a])
        img = Image.fromarray((rgba * 255).astype(np.uint8), "RGBA")
    img.save(os.path.join(OUT, name))
    print(f"{name}: {img.size}")


N = 256
u = np.linspace(0, 1, N, endpoint=False)
V, U = np.meshgrid(u, u)  # V — строки (v), U — столбцы (u)

# ——— трава: пятнистый тон + вертикальные травинки + сухие проплешины ———
tone = fbm(N, 6, 4, 11)
blades = fbm(N, 48, 2, 12)
# травинки: вертикально вытянутый шум — растяжение по u
blades_v = lattice(N, 96, 13)  # высокая частота
stretch = np.zeros((N, N))
for i in range(0, N, 3):  # колонки по 1-2 px — вертикальные штрихи
    stretch[:, i : min(i + 2, N)] = blades_v[:, i : min(i + 2, N)]
dry = fbm(N, 3, 3, 14)
g = 0.34 + 0.13 * (tone - 0.5) + 0.10 * (blades - 0.5) + 0.07 * (stretch - 0.5)
r_ = g * 0.62
b_ = g * 0.42
drymask = np.clip((dry - 0.62) * 5, 0, 1)
r_ = r_ * (1 - drymask) + drymask * (g * 1.35 + 0.10)
b_ = b_ * (1 - drymask) + drymask * (g * 0.30)
save("grass.png", np.dstack([r_, g, b_]))

# ——— песок: зерно + рябь дюн ———
grain = rng.random((N, N)) * 0.06 - 0.03
ripple = 0.5 + 0.5 * np.sin(2 * np.pi * (5 * V + 1.2 * fbm(N, 4, 3, 21)))
ripple2 = 0.5 + 0.5 * np.sin(2 * np.pi * (11 * U - 0.8 * fbm(N, 5, 2, 22)))
speck = (rng.random((N, N)) > 0.995) * -0.10
s = 0.76 + 0.05 * (ripple - 0.5) + 0.03 * (ripple2 - 0.5) + grain + speck
save("sand.png", np.dstack([s * 1.00, s * 0.90, s * 0.70]))

# ——— камень: слои-пласты + трещины + лишайник ———
warp = fbm(N, 4, 3, 31)
strata = 0.5 + 0.5 * np.sin(2 * np.pi * (7 * V + 2.0 * warp))
fine = fbm(N, 24, 3, 32)
crackn = fbm(N, 9, 4, 33)
crack = np.clip(1 - np.abs(crackn - 0.5) * 14, 0, 1)  # ridged
lichen = np.clip((fbm(N, 5, 3, 34) - 0.60) * 4, 0, 1)
k = 0.50 + 0.09 * (strata - 0.5) * 2 + 0.10 * (fine - 0.5)
k = k - 0.16 * crack
r_ = k * 1.05
g_ = k * 0.97
b_ = k * 0.88
r_ = r_ * (1 - lichen) + lichen * (k * 0.75)
g_ = g_ * (1 - lichen) + lichen * (k * 1.05)
b_ = b_ * (1 - lichen) + lichen * (k * 0.55)
save("rock.png", np.dstack([r_, g_, b_]))

# ——— кора: вертикальные борозды ———
furrow = 0.5 + 0.5 * np.sin(2 * np.pi * (14 * U + 0.6 * fbm(N, 3, 2, 41)))
depth = fbm(N, 8, 2, 42)
k = 0.52 + 0.16 * (furrow - 0.5) + 0.10 * (depth - 0.5)
save("bark.png", np.dstack([k * 1.10, k * 0.80, k * 0.55]))

# ——— листва (RGBA): МЯГКИЙ кластер с пером по краю и прорехами ———
M = 128
um = np.linspace(0, 1, M, endpoint=False)
VM, UM = np.meshgrid(um, um)
cx, cy = 0.5, 0.5
d = np.sqrt(((UM - cx) * 2.1) ** 2 + ((VM - cy) * 2.1) ** 2)
wob = fbm(M, 7, 3, 51)
body = np.clip((0.85 - d + (wob - 0.5) * 0.55) * 4.0, 0, 1)  # мягкий край
holes = np.clip((fbm(M, 10, 3, 52) - 0.62) * 6, 0, 1) * np.clip(d * 2.2, 0, 1)
alpha = np.clip(body - holes, 0, 1)
lt = fbm(M, 9, 3, 53)
g_ = 0.34 + 0.16 * (lt - 0.5)
r_ = g_ * 0.55
b_ = g_ * 0.40
# край чуть светлее (подсветка кроны)
edge = np.clip((d - 0.28) * 3, 0, 1) * 0.25
save("leaf.png", np.dstack([r_ + edge, g_ + edge * 1.2, b_ + edge * 0.5]), alpha)

# ——— лист пальмы (RGBA): рахис + листки веером ———
W2, H2 = 256, 128
up = np.linspace(0, 1, W2, endpoint=False)
vp = np.linspace(0, 1, H2, endpoint=False)
UU, VV = np.meshgrid(up, vp)  # VV 0..1 поперёк, UU вдоль
rib = np.exp(-((VV - 0.5) ** 2) / 0.0004)  # центральный стержень
# листки: наклонные полосы от стержня, ширина растёт к кончику
leaflets = np.sin(2 * np.pi * 46 * (VV - 0.5))
taper = np.clip(1 - np.abs(VV - 0.5) * 2.4, 0, 1)
body = np.clip((0.75 - np.abs(VV - 0.5) * 2.2 + 0.06 * np.sin(2 * np.pi * 3 * UU)) * 3.0, 0, 1)
alpha_p = np.clip(body * (0.35 + 0.65 * np.abs(leaflets) ** 0.3) + rib * 0.9, 0, 1)
fade = np.clip((1 - UU) * 4, 0, 1) * np.clip(UU * 8 + 0.15, 0, 1)  # основание и кончик
alpha_p = alpha_p * fade
g_ = 0.40 + 0.10 * np.sin(2 * np.pi * 9 * UU) * 0.3 + (VV - 0.5) * 0.08
save("palm.png", np.dstack([g_ * 0.58, g_, g_ * 0.42]), alpha_p)

# ——— облако (RGBA): гроздь мягких гауссиан ———
CW, CH = 256, 128
uc = np.linspace(0, 1, CW, endpoint=False)
vc = np.linspace(0, 1, CH, endpoint=False)
UUC, VVC = np.meshgrid(uc, vc)
alpha_c = np.zeros((CH, CW))
for i in range(14):
    px = 0.18 + 0.64 * rng.random()
    py = 0.42 + 0.22 * rng.random()
    sx = 0.06 + 0.10 * rng.random()
    sy = sx * (0.55 + 0.3 * rng.random())
    alpha_c += 0.55 * np.exp(-(((UUC - px) / sx) ** 2 + ((VVC - py) / sy) ** 2))
alpha_c = np.clip(alpha_c, 0, 0.92)
shade_c = 0.92 + 0.08 * (VVC - 0.5)
save("cloud.png", np.dstack([shade_c, shade_c, shade_c * 1.02]), alpha_c)

# ——— свечение (RGBA): радиальный спад — для солнца и мягких теней ———
G = 64
ug = np.linspace(-1, 1, G, endpoint=False)
UG, VG = np.meshgrid(ug, ug)
r_ = np.sqrt(UG**2 + VG**2) / 1.0
a_g = (np.clip(1 - r_, 0, 1) ** 2.6) * 0.5
save("glow.png", np.ones((G, G, 3)), a_g)

# ——— пучок травы (RGBA): билборд-детализация ———
T = 64
ut = np.linspace(0, 1, T, endpoint=False)
ut2 = np.linspace(0, 1, T, endpoint=False)
UUT, VVT = np.meshgrid(ut, ut2)
alpha_t = np.zeros((T, T))
rs = np.random.default_rng(77)
for i in range(17):
    bx = 0.18 + 0.64 * rs.random()
    lean = (rs.random() - 0.5) * 0.5
    hgt = 0.55 + 0.40 * rs.random()
    xx = UUT - bx - lean * (1 - VVT) * (1 - VVT)
    blade = np.exp(-((xx / 0.012) ** 2)) * ((VVT < hgt) & (VVT > 0.04))
    alpha_t = np.maximum(alpha_t, blade)
gt = 0.30 + 0.22 * rs.random((T, T)) * (VVT < 0.95)
save("tuft.png", np.dstack([gt * 0.60, gt, gt * 0.35]), alpha_t)

print("готово:", sorted(os.listdir(OUT)))
