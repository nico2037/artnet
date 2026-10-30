"""Volumetric moving head demo: automatic timeline, or --manual for direct keyboard control."""
import argparse
import math
import time
from pathlib import Path

import moderngl
import numpy as np
import pygame
from PIL import Image

from fixtures import aim_at, create_fixtures
from mathutil import box, look_at, perspective, quad
from renderer import QUALITY, Renderer

ROOT = Path(__file__).resolve().parent
WIDTH, HEIGHT = 1280, 720
EYE = (0.0, 4.0, 12.0)
TARGET = (0.0, 2.2, -3.0)
FOV = 50.0
TRUSS_Y = 6.2          # truss underside
PER_ROW = 25
LOOP = 38.0            # seconds
BLINDER = (29.0, 37.0)  # all beams aim into the camera
PAN_SPEED = 60.0       # degrees per second while an arrow key is held
ROTATE_STEP = 15.0     # gobo/prism rotation speed change per key press, degrees per second
ZOOM_SPEED = 15.0      # degrees per second
FOCUS_SPEED = 0.3      # focus units per second
WHEEL_SECONDS_PER_SLOT = 0.35

MANUAL_HELP = """Manual mode (all fixtures together):
  Arrows  pan / tilt           G  next gobo slot (wheel 1)
  Q / E   gobo rotation slower / faster (per press, continuous)
  A / D   prism rotation slower / faster (per press, continuous)
  H       next gobo slot (wheel 2)     P  prism in / out
  W / S   zoom wider / narrower
  R / F   focus (wheel 1 sharp at 0.0, wheel 2 at 0.3)
  C       next color"""


def build_stage(truss_rows):
    parts = [
        (quad((-15, 0, -8), (15, 0, -8), (15, 0, 14), (-15, 0, 14), (0, 1, 0)), 0.3),
        (quad((-15, 0, -8), (15, 0, -8), (15, 12, -8), (-15, 12, -8), (0, 0, 1)), 0.4),
    ]
    parts += [(box((-7, TRUSS_Y, z - 0.15), (7, TRUSS_Y + 0.3, z + 0.15)), 0.35) for z in truss_rows]
    parts += [(box((x - 0.2, 0, -4.7), (x + 0.2, 3.2, -4.3)), 0.35) for x in (-3.0, 0.0, 3.0)]
    return np.vstack([np.hstack([p, n, np.full((len(p), 3), albedo)]) for (p, n), albedo in parts])


def create_rig(count):
    rows = math.ceil(count / PER_ROW)
    row_z = -2.0 + (np.arange(rows) - (rows - 1) / 2) * 1.4
    fx = create_fixtures(count)
    for i in range(count):
        r, c = divmod(i, PER_ROW)
        in_row = min(PER_ROW, count - r * PER_ROW)
        x = 0.0 if in_row == 1 else -6.0 + 12.0 * c / (in_row - 1)
        fx["pos"][i] = (x, TRUSS_Y - 0.52, row_z[r])
    return fx, row_z


def _window(t, start, end, fade=1.5):
    rise = np.clip((t - start) / fade, 0.0, 1.0)
    fall = np.clip((end - t) / fade, 0.0, 1.0)
    w = min(rise, fall)
    return w * w * (3 - 2 * w)


def _wheel_position(tl, schedule, slots, seconds_per_slot=WHEEL_SECONDS_PER_SLOT):
    """schedule: [(time, slot per fixture)], first at 0; the wheel travels forward slot by slot."""
    k = np.searchsorted([t for t, _ in schedule], tl, side="right") - 1
    start, (t_change, target) = schedule[k - 1][1], schedule[k]
    delta = (target - start) % slots
    travel = np.clip((tl - t_change) / (seconds_per_slot * np.maximum(delta, 1)), 0.0, 1.0)
    return start + delta * travel * travel * (3 - 2 * travel)


PALETTE = np.array([[1.0, 1.0, 1.0], [1.0, 0.25, 0.1], [0.2, 0.5, 1.0],
                    [1.0, 0.8, 0.3], [0.3, 1.0, 0.5], [0.9, 0.3, 1.0]])


def animate(fx, t, gobo_count):
    # every fixture gets its own state (fixture 0 = reference demo) so no beam shares work with another
    i = np.arange(len(fx))
    tl = t % LOOP
    tp = t + 0.37 * i
    blind = _window(tl, *BLINDER, fade=2.5)
    # fanned sweep across the audience at eye height, so beams pass the camera one after another
    phase = 2 * math.pi * (tl - BLINDER[0]) / 6.0 + 0.4 * (i // PER_ROW)
    sweep_x = 2.0 * fx["pos"][:, 0] + 8.0 * np.sin(phase)
    targets = np.column_stack([sweep_x, np.full(len(fx), EYE[1]), np.full(len(fx), EYE[2])])
    blind_pan, blind_tilt = aim_at(fx, targets)
    sweep_pan = 40.0 * np.sin(2 * math.pi * tp / 16.0)
    sweep_tilt = 5.0 + 30.0 * np.sin(2 * math.pi * tp / 13.0)
    fx["pan"] = sweep_pan + (blind_pan - sweep_pan) * blind
    fx["tilt"] = sweep_tilt + (blind_tilt - sweep_tilt) * blind
    blinder_phase = tl >= BLINDER[0] - 1.0
    open_slot = np.zeros(len(fx))
    first, second = (1 + i % gobo_count, 1 + (i + 1) % gobo_count) if gobo_count else (open_slot, open_slot)
    schedule = [(0.0, open_slot), (6.0, first), (20.0, second), (BLINDER[0] - 1.0, open_slot)]
    fx["gobo"] = _wheel_position(tl, schedule, gobo_count + 1)
    fx["gobo_rot"] = np.where(i % 2, -1.0, 1.0) * (70.0 + 15.0 * (i % 5)) * t
    # wheel 2 joins for a focus morph: wheel 1 sharp -> wheel 2 sharp -> back
    third = 1 + (i + 3) % gobo_count if gobo_count else open_slot
    fx["gobo2"] = _wheel_position(tl, [(0.0, open_slot), (8.5, third), (13.0, open_slot)], gobo_count + 1)
    fx["gobo2_rot"] = 0.0
    prism_on = (13.0 <= tl < 28.0) != (i % 2 == 1)
    fx["prism"] = np.where(prism_on & (not blinder_phase), 3, 0)
    fx["prism_rot"] = -45.0 * (1.0 + 0.3 * (i % 3)) * t
    zoom = 14.0 + 10.0 * _window(tl, 20.0, 26.0)
    fx["zoom"] = zoom + (4.0 - zoom) * blind
    fx["focus"] = 0.05 + 0.25 * _window(tl, 9.5, 12.5, fade=1.0)
    fx["color"] = PALETTE[i % len(PALETTE)]


def _step_wheel(pos, target, slots, dt):
    """Moves a wheel forward toward its target slot at WHEEL_SECONDS_PER_SLOT."""
    remaining = (target - pos) % slots
    if remaining > slots - 1e-3:  # float noise right at the target, not a full turn
        remaining = 0.0
    return (pos + min(remaining, dt / WHEEL_SECONDS_PER_SLOT)) % slots


class ManualControl:
    """Static look driven by keys; pan/tilt come from the arrow-key offsets in the main loop."""

    def __init__(self, gobo_count):
        self.slots = gobo_count + 1
        self.gobo = self.gobo2 = 0.0
        self.gobo_target = self.gobo2_target = 0
        self.gobo_rot = self.prism_rot = 0.0
        self.gobo_speed = self.prism_speed = 0.0
        self.prism = False
        self.zoom = 14.0
        self.focus = 0.05
        self.color = 0

    def key_down(self, key):
        if key == pygame.K_g:
            self.gobo_target = (self.gobo_target + 1) % self.slots
        elif key == pygame.K_h:
            self.gobo2_target = (self.gobo2_target + 1) % self.slots
        elif key == pygame.K_p:
            self.prism = not self.prism
        elif key == pygame.K_c:
            self.color = (self.color + 1) % len(PALETTE)
        elif key in (pygame.K_q, pygame.K_e):
            self.gobo_speed += ROTATE_STEP if key == pygame.K_e else -ROTATE_STEP
        elif key in (pygame.K_a, pygame.K_d):
            self.prism_speed += ROTATE_STEP if key == pygame.K_d else -ROTATE_STEP

    def update(self, keys, dt):
        def axis(plus, minus):
            return keys[plus] - keys[minus]

        self.gobo_rot += self.gobo_speed * dt
        self.prism_rot += self.prism_speed * dt
        self.zoom = min(60.0, max(2.0, self.zoom + ZOOM_SPEED * dt * axis(pygame.K_w, pygame.K_s)))
        self.focus = min(1.0, max(0.0, self.focus + FOCUS_SPEED * dt * axis(pygame.K_r, pygame.K_f)))
        self.gobo = _step_wheel(self.gobo, self.gobo_target, self.slots, dt)
        self.gobo2 = _step_wheel(self.gobo2, self.gobo2_target, self.slots, dt)

    def apply(self, fx):
        fx["pan"] = 0.0
        fx["tilt"] = 0.0
        fx["gobo"] = self.gobo
        fx["gobo_rot"] = self.gobo_rot
        fx["gobo2"] = self.gobo2
        fx["gobo2_rot"] = 0.0
        fx["prism"] = 3 if self.prism else 0
        fx["prism_rot"] = self.prism_rot
        fx["zoom"] = self.zoom
        fx["focus"] = self.focus
        fx["color"] = PALETTE[self.color]

    def status(self):
        return (f"gobo {self.gobo_target}/{self.gobo2_target} {self.gobo_speed:+.0f}°/s | "
                f"prism {'in' if self.prism else 'out'} {self.prism_speed:+.0f}°/s | "
                f"zoom {self.zoom:.0f}° | focus {self.focus:.2f}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--quality", choices=list(QUALITY), default="medium")
    parser.add_argument("--fixtures", type=int, default=1, help="stress test only")
    parser.add_argument("--screenshot", type=Path, help="render one frame to this PNG and exit")
    parser.add_argument("--time", type=float, default=18.0, help="timeline position for --screenshot")
    parser.add_argument("--manual", action="store_true", help="no animation, control the beam with the keyboard")
    parser.add_argument("--target-fps", type=float, default=60.0,
                        help="lower the haze resolution below the quality preset while FPS is below this (0 = off)")
    args = parser.parse_args()

    pygame.init()
    pygame.display.gl_set_attribute(pygame.GL_CONTEXT_MAJOR_VERSION, 3)
    pygame.display.gl_set_attribute(pygame.GL_CONTEXT_MINOR_VERSION, 3)
    pygame.display.gl_set_attribute(pygame.GL_CONTEXT_PROFILE_MASK, pygame.GL_CONTEXT_PROFILE_CORE)
    pygame.display.gl_set_attribute(pygame.GL_CONTEXT_FLAGS, pygame.GL_CONTEXT_FORWARD_COMPATIBLE_FLAG)
    pygame.display.set_mode((WIDTH, HEIGHT), pygame.OPENGL | pygame.DOUBLEBUF)
    pygame.display.set_caption("Movinghead Demo")
    ctx = moderngl.create_context()

    fixtures, rows = create_rig(max(1, args.fixtures))
    size = ctx.screen.size
    renderer = Renderer(ctx, size, args.quality, ROOT / "gobos", build_stage(rows))
    view = look_at(EYE, TARGET)
    proj = perspective(FOV, size[0] / size[1], 0.1, 200.0)

    if args.screenshot:
        animate(fixtures, args.time, renderer.gobo_count)
        renderer.render(fixtures, EYE, view, proj, args.time)
        image = Image.frombytes("RGB", size, ctx.screen.read(components=3))
        image.transpose(Image.FLIP_TOP_BOTTOM).save(args.screenshot)
        pygame.quit()
        return

    start = last = prev = time.perf_counter()
    frames = 0
    pan_offset = tilt_offset = 0.0
    manual = ManualControl(renderer.gobo_count) if args.manual else None
    if manual:
        print(MANUAL_HELP)
    running = True
    while running:
        for event in pygame.event.get():
            if event.type == pygame.QUIT or (event.type == pygame.KEYDOWN and event.key == pygame.K_ESCAPE):
                running = False
            elif event.type == pygame.KEYDOWN and manual:
                manual.key_down(event.key)

        now = time.perf_counter()
        dt = now - prev
        prev = now
        keys = pygame.key.get_pressed()
        pan_offset += PAN_SPEED * dt * (keys[pygame.K_RIGHT] - keys[pygame.K_LEFT])
        # hanging heads: negative tilt lifts the beam toward the audience
        tilt_offset += PAN_SPEED * dt * (keys[pygame.K_DOWN] - keys[pygame.K_UP])

        t = now - start
        if manual:
            manual.update(keys, dt)
            manual.apply(fixtures)
        else:
            animate(fixtures, t, renderer.gobo_count)
        fixtures["pan"] += pan_offset
        fixtures["tilt"] += tilt_offset
        beams = renderer.render(fixtures, EYE, view, proj, t)
        pygame.display.flip()

        frames += 1
        if now - last >= 0.5:
            fps = frames / (now - last)
            if args.target_fps:
                renderer.adapt_volume_scale(fps, args.target_fps)
            mode = f"MANUAL | {manual.status()}" if manual else "demo"
            pygame.display.set_caption(
                f"Movinghead Demo | {fps:.0f} FPS | {args.quality} haze {renderer.volume_scale:.0%} | "
                f"{len(fixtures)} fixtures, {beams} beams | pan {pan_offset:+.0f}° tilt {tilt_offset:+.0f}° | {mode}")
            frames, last = 0, now
    pygame.quit()


if __name__ == "__main__":
    main()
