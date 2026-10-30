"""Fixture state (one row per moving head) and conversion to GPU instance data.

The state array is the single source of truth; Art-Net/DMX will later write into it.
"""
import numpy as np

from mathutil import rot_x, rot_y, rot_z

FIXTURE_DTYPE = np.dtype([
    ("pos", "f4", 3),          # head center (tilt axis), world
    ("hanging", "?"),
    ("pan", "f4"),             # degrees
    ("tilt", "f4"),            # degrees, 0 = beam points away from base
    ("dimmer", "f4"),          # 0..1
    ("color", "f4", 3),        # linear RGB 0..1
    ("zoom", "f4"),            # full beam angle, degrees
    ("focus", "f4"),           # 0..1, selects the sharp plane (see WHEEL_PLANES)
    ("gobo", "f4"),            # wheel position: 0 open, n = gobo n-1, fraction = wheel in motion
    ("gobo_rot", "f4"),        # degrees
    ("gobo2", "f4"),           # second gobo wheel, same slot convention
    ("gobo2_rot", "f4"),       # degrees
    ("prism", "i4"),           # facet count, 0 = out
    ("prism_rot", "f4"),       # degrees
    ("prism_spread", "f4"),    # deflection per facet, degrees
])

LENS_OFFSET = 0.14
BEAM_LENGTH = 40.0
BEAM_FLOATS = 22   # apex 3, basis 9, cone 3 (tan half angle, length, focus), color 3, shadow origin 3 + slot 1
BODY_FLOATS = 33   # pos 3, mount 9, pan 9, beam 9, lens color 3
LIGHT_FLOATS = 7   # slot 1, wheel 1 (pos, rot, blur) 3, wheel 2 3
WHEEL_PLANES = (0.0, 0.3)  # focus value at which each gobo wheel is sharp
FOCUS_BLUR = 8.0           # mip levels of blur per unit of defocus

_TILT_ZERO = rot_x(-np.pi / 2)  # maps beam axis +Z onto fixture +Y


def create_fixtures(count):
    fx = np.zeros(count, FIXTURE_DTYPE)
    fx["hanging"] = True
    fx["dimmer"] = 1.0
    fx["color"] = 1.0
    fx["zoom"] = 14.0
    fx["prism_spread"] = 7.0
    return fx


def _frames(state):
    n = len(state)
    mount = np.where(state["hanging"][:, None, None], rot_x(np.full(n, np.pi)), np.eye(3))
    pan_frame = mount @ rot_y(np.radians(state["pan"]))
    beam = pan_frame @ rot_x(np.radians(state["tilt"])) @ _TILT_ZERO
    return mount, pan_frame, beam


def aim_at(state, target):
    """Pan/tilt (degrees) that point each fixture's beam at a world position."""
    d = np.asarray(target, np.float64) - state["pos"]
    d /= np.linalg.norm(d, axis=1, keepdims=True)
    flip = np.where(state["hanging"], -1.0, 1.0)
    dx, dy, dz = d[:, 0], d[:, 1] * flip, d[:, 2] * flip
    return np.degrees(np.arctan2(-dx, -dz)), -np.degrees(np.arccos(np.clip(dy, -1.0, 1.0)))


def _cols(m):
    return m.transpose(0, 2, 1).reshape(len(m), 9)


def build_body_instances(state):
    if len(state) == 0:
        return np.zeros((0, BODY_FLOATS), np.float32)
    mount, pan_frame, beam = _frames(state)
    lens = state["color"] * state["dimmer"][:, None]
    return np.hstack([state["pos"], _cols(mount), _cols(pan_frame), _cols(beam), lens]).astype(np.float32)


def build_light_instances(state):
    """Per fixture: everything in front of the lens, composed into one light image tile."""
    def blur(plane):
        return np.abs(state["focus"] - plane) * FOCUS_BLUR

    return np.column_stack([
        np.arange(len(state)),
        state["gobo"], np.radians(state["gobo_rot"]), blur(WHEEL_PLANES[0]),
        state["gobo2"], np.radians(state["gobo2_rot"]), blur(WHEEL_PLANES[1]),
    ]).astype(np.float32)


def build_beam_instances(state):
    """One cone instance per beam; an active prism splits a fixture into one beam per facet."""
    lit_index = np.nonzero(state["dimmer"] > 1e-3)[0]
    lit = state[lit_index]
    if len(lit) == 0:
        return np.zeros((0, BEAM_FLOATS), np.float32)
    _, _, base = _frames(lit)

    facets = np.where(lit["prism"] > 0, lit["prism"], 1)
    idx = np.repeat(np.arange(len(lit)), facets)
    k = np.arange(len(idx)) - np.repeat(np.cumsum(facets) - facets, facets)
    f = facets[idx]

    phi = np.radians(lit["prism_rot"][idx]) + 2 * np.pi * k / f
    spread = np.where(lit["prism"][idx] > 0, np.radians(lit["prism_spread"][idx]), 0.0)
    basis = base[idx] @ rot_z(phi) @ rot_y(spread) @ rot_z(-phi)

    apex = lit["pos"][idx] + base[idx][:, :, 2] * LENS_OFFSET
    cone = np.stack([np.tan(np.radians(lit["zoom"][idx]) / 2), np.full(len(idx), BEAM_LENGTH), lit["focus"][idx]], 1)
    color = lit["color"][idx] * (lit["dimmer"][idx] / f)[:, None]
    # slot = fixture index: selects both the light image tile and the shadow maps;
    # shadow maps are centered on the head (not the lens), so they stay valid while the head moves
    shadow = np.hstack([lit["pos"][idx], lit_index[idx, None]])
    return np.hstack([apex, _cols(basis), cone, color, shadow]).astype(np.float32)
