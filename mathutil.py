import numpy as np


def perspective(fov_y_deg, aspect, near, far):
    f = 1.0 / np.tan(np.radians(fov_y_deg) / 2.0)
    m = np.zeros((4, 4), np.float64)
    m[0, 0] = f / aspect
    m[1, 1] = f
    m[2, 2] = (far + near) / (near - far)
    m[2, 3] = 2.0 * far * near / (near - far)
    m[3, 2] = -1.0
    return m


def look_at(eye, target, up=(0.0, 1.0, 0.0)):
    eye, target, up = (np.asarray(v, np.float64) for v in (eye, target, up))
    f = target - eye
    f /= np.linalg.norm(f)
    s = np.cross(f, up)
    s /= np.linalg.norm(s)
    u = np.cross(s, f)
    m = np.eye(4)
    m[0, :3], m[1, :3], m[2, :3] = s, u, -f
    m[:3, 3] = -m[:3, :3] @ eye
    return m


def _rot(angle, i, j):
    a = np.asarray(angle, np.float64)
    c, s = np.cos(a), np.sin(a)
    m = np.zeros(a.shape + (3, 3))
    k = 3 - i - j
    m[..., k, k] = 1.0
    m[..., i, i] = c
    m[..., j, j] = c
    m[..., i, j] = -s
    m[..., j, i] = s
    return m


def rot_x(angle):
    return _rot(angle, 1, 2)


def rot_y(angle):
    return _rot(angle, 2, 0)


def rot_z(angle):
    return _rot(angle, 0, 1)


# Meshes: triangles are oriented CCW seen from outside (needed for back-face-only beam passes).

def _oriented(verts, faces, center):
    tri = np.asarray(verts, np.float64)[np.asarray(faces)]
    n = np.cross(tri[:, 1] - tri[:, 0], tri[:, 2] - tri[:, 0])
    flip = np.einsum("ij,ij->i", n, tri.mean(axis=1) - center) < 0
    tri[flip] = tri[flip][:, ::-1]
    n[flip] = -n[flip]
    n /= np.linalg.norm(n, axis=1, keepdims=True)
    return tri.reshape(-1, 3), np.repeat(n, 3, axis=0)


_BOX_FACES = [(0, 1, 3), (0, 3, 2), (4, 5, 7), (4, 7, 6), (0, 1, 5), (0, 5, 4),
              (2, 3, 7), (2, 7, 6), (0, 2, 6), (0, 6, 4), (1, 3, 7), (1, 7, 5)]


def box(lo, hi):
    lo, hi = np.asarray(lo, np.float64), np.asarray(hi, np.float64)
    corners = [[hi[0] if i & 1 else lo[0], hi[1] if i & 2 else lo[1], hi[2] if i & 4 else lo[2]]
               for i in range(8)]
    return _oriented(corners, _BOX_FACES, (lo + hi) / 2)


def quad(p0, p1, p2, p3, normal):
    pos = np.array([p0, p1, p2, p0, p2, p3], np.float64)
    return pos, np.tile(np.asarray(normal, np.float64), (6, 1))


def disc(z, radius, segments):
    ang = np.linspace(0, 2 * np.pi, segments, endpoint=False)
    verts = np.vstack([[0, 0, z], np.stack([radius * np.cos(ang), radius * np.sin(ang), np.full(segments, z)], 1)])
    faces = [(0, 1 + i, 1 + (i + 1) % segments) for i in range(segments)]
    return _oriented(verts, faces, np.array([0, 0, z - 1.0]))


def cone_mesh(segments):
    """Closed unit cone: apex at origin, axis +Z, radius 1 at z=1."""
    ang = np.linspace(0, 2 * np.pi, segments, endpoint=False)
    ring = np.stack([np.cos(ang), np.sin(ang), np.ones(segments)], 1)
    verts = np.vstack([[0, 0, 0], [0, 0, 1], ring])
    faces = []
    for i in range(segments):
        a, b = 2 + i, 2 + (i + 1) % segments
        faces += [(0, a, b), (1, b, a)]
    pos, _ = _oriented(verts, faces, np.array([0, 0, 0.7]))
    return pos


def fixture_body_mesh():
    """Vertices (pos, normal, part). Parts: 0 base, 1 yoke, 2 head, 3 lens."""
    parts = [
        (0, box((-0.2, -0.52, -0.2), (0.2, -0.4, 0.2))),
        (1, box((-0.25, -0.4, -0.05), (-0.2, 0.02, 0.05))),
        (1, box((0.2, -0.4, -0.05), (0.25, 0.02, 0.05))),
        (2, box((-0.17, -0.17, -0.28), (0.17, 0.17, 0.13))),
        (3, disc(0.135, 0.13, 24)),
    ]
    return np.vstack([np.hstack([p, n, np.full((len(p), 1), part)]) for part, (p, n) in parts])
