"""Generates sample gobo PNGs (white = light passes) into ../gobos."""
import math
import random
from pathlib import Path

from PIL import Image, ImageDraw

SIZE = 256
SUPERSAMPLE = 4
OUT = Path(__file__).resolve().parent.parent / "gobos"


def canvas(background=0):
    img = Image.new("L", (SIZE * SUPERSAMPLE,) * 2, background)
    return img, ImageDraw.Draw(img), SIZE * SUPERSAMPLE / 2


def save(img, name):
    aperture = Image.new("L", img.size, 0)
    ImageDraw.Draw(aperture).ellipse([0, 0, img.size[0] - 1, img.size[1] - 1], fill=255)
    img = Image.composite(img, Image.new("L", img.size, 0), aperture)
    img.resize((SIZE, SIZE), Image.LANCZOS).save(OUT / f"{name}.png")


def star(points=5):
    img, d, c = canvas()
    pts = []
    for i in range(points * 2):
        r = c * (0.92 if i % 2 == 0 else 0.38)
        a = math.pi * i / points - math.pi / 2
        pts.append((c + r * math.cos(a), c + r * math.sin(a)))
    d.polygon(pts, fill=255)
    save(img, "01_star")


def dot_ring(count=8):
    img, d, c = canvas()
    for i in range(count):
        a = 2 * math.pi * i / count
        x, y, r = c + c * 0.62 * math.cos(a), c + c * 0.62 * math.sin(a), c * 0.17
        d.ellipse([x - r, y - r, x + r, y + r], fill=255)
    r = c * 0.2
    d.ellipse([c - r, c - r, c + r, c + r], fill=255)
    save(img, "02_dots")


def bars(count=5):
    img, d, c = canvas()
    width = c * 2 / (count * 2 - 1)
    for i in range(count):
        x = i * 2 * width
        d.rectangle([x, 0, x + width, c * 2], fill=255)
    save(img, "03_bars")


def breakup(blobs=45):
    img, d, c = canvas(255)
    rng = random.Random(7)
    for _ in range(blobs):
        x, y = rng.uniform(0, 2 * c), rng.uniform(0, 2 * c)
        rx, ry = rng.uniform(0.05, 0.16) * c, rng.uniform(0.05, 0.16) * c
        d.ellipse([x - rx, y - ry, x + rx, y + ry], fill=0)
    save(img, "04_breakup")


if __name__ == "__main__":
    OUT.mkdir(exist_ok=True)
    star()
    dot_ring()
    bars()
    breakup()
    print(f"Gobos written to {OUT}")
