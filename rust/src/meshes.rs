use std::f32::consts::PI;

use anyhow::{anyhow, Context, Result};
use glam::{Mat4, Vec3};

/// Triangle soup with flat normals; triangles are CCW seen from outside (the beam passes rely on it).
pub struct Mesh {
    pub pos: Vec<Vec3>,
    pub normal: Vec<Vec3>,
}

fn oriented(verts: &[Vec3], faces: &[[usize; 3]], center: Vec3) -> Mesh {
    let mut mesh = Mesh { pos: Vec::new(), normal: Vec::new() };
    for f in faces {
        let (mut a, b, mut c) = (verts[f[0]], verts[f[1]], verts[f[2]]);
        let mut n = (b - a).cross(c - a);
        if n.dot((a + b + c) / 3.0 - center) < 0.0 {
            std::mem::swap(&mut a, &mut c);
            n = -n;
        }
        let n = n.normalize();
        mesh.pos.extend([a, b, c]);
        mesh.normal.extend([n; 3]);
    }
    mesh
}

const BOX_FACES: [[usize; 3]; 12] = [
    [0, 1, 3], [0, 3, 2], [4, 5, 7], [4, 7, 6], [0, 1, 5], [0, 5, 4],
    [2, 3, 7], [2, 7, 6], [0, 2, 6], [0, 6, 4], [1, 3, 7], [1, 7, 5],
];

pub fn cuboid(lo: Vec3, hi: Vec3) -> Mesh {
    let corners: Vec<Vec3> = (0..8)
        .map(|i| {
            Vec3::new(
                if i & 1 != 0 { hi.x } else { lo.x },
                if i & 2 != 0 { hi.y } else { lo.y },
                if i & 4 != 0 { hi.z } else { lo.z },
            )
        })
        .collect();
    oriented(&corners, &BOX_FACES, (lo + hi) / 2.0)
}

pub fn quad(p: [Vec3; 4], normal: Vec3) -> Mesh {
    Mesh { pos: vec![p[0], p[1], p[2], p[0], p[2], p[3]], normal: vec![normal; 6] }
}

fn ring(segments: usize, radius: f32, z: f32) -> impl Iterator<Item = Vec3> {
    (0..segments).map(move |i| {
        let a = std::f32::consts::TAU * i as f32 / segments as f32;
        Vec3::new(radius * a.cos(), radius * a.sin(), z)
    })
}

pub fn disc(z: f32, radius: f32, segments: usize) -> Mesh {
    let verts: Vec<Vec3> = std::iter::once(Vec3::new(0.0, 0.0, z)).chain(ring(segments, radius, z)).collect();
    let faces: Vec<[usize; 3]> = (0..segments).map(|i| [0, 1 + i, 1 + (i + 1) % segments]).collect();
    oriented(&verts, &faces, Vec3::new(0.0, 0.0, z - 1.0))
}

/// Closed unit cone: apex at origin, axis +Z, radius 1 at z=1.
pub fn cone_mesh(segments: usize) -> Vec<Vec3> {
    let verts: Vec<Vec3> = [Vec3::ZERO, Vec3::Z].into_iter().chain(ring(segments, 1.0, 1.0)).collect();
    let mut faces = Vec::new();
    for i in 0..segments {
        let (a, b) = (2 + i, 2 + (i + 1) % segments);
        faces.push([0, a, b]);
        faces.push([1, b, a]);
    }
    oriented(&verts, &faces, Vec3::new(0.0, 0.0, 0.7)).pos
}

/// Interleaved (pos 3, normal 3, part 1). Parts: 0 base, 1 yoke, 2 head, 3 lens.
pub fn fixture_body_mesh() -> Vec<f32> {
    let v = Vec3::new;
    let parts = [
        (0.0, cuboid(v(-0.2, -0.52, -0.2), v(0.2, -0.4, 0.2))),
        (1.0, cuboid(v(-0.25, -0.4, -0.05), v(-0.2, 0.02, 0.05))),
        (1.0, cuboid(v(0.2, -0.4, -0.05), v(0.25, 0.02, 0.05))),
        (2.0, cuboid(v(-0.17, -0.17, -0.28), v(0.17, 0.17, 0.13))),
        (3.0, disc(0.135, 0.13, 24)),
    ];
    let mut out = Vec::new();
    for (part, mesh) in parts {
        for (p, n) in mesh.pos.iter().zip(&mesh.normal) {
            out.extend([p.x, p.y, p.z, n.x, n.y, n.z, part]);
        }
    }
    out
}

/// Triangle list of a 3DS or glTF/GLB file (by extension). 3DS keeps its file units and axes;
/// glTF (Y-up) is turned Z-up so both match MVR and GDTF axes, in the file's units.
pub fn file_triangles(name: &str, data: &[u8]) -> Result<Vec<Vec3>> {
    let lower = name.to_lowercase();
    if lower.ends_with(".3ds") {
        let parsed = ds3::parse(data).map_err(|e| anyhow!("{e}"))?;
        let mut tris = Vec::new();
        for mesh in &parsed.meshes {
            for f in &mesh.faces {
                tris.extend(f.iter().map(|&i| Vec3::from(mesh.vertices[i as usize])));
            }
        }
        Ok(tris)
    } else if lower.ends_with(".glb") || lower.ends_with(".gltf") {
        let (doc, buffers, _) = gltf::import_slice(data).context("glTF")?;
        let mut tris = Vec::new();
        let z_up = Mat4::from_rotation_x(PI / 2.0);
        let scene_root = doc.default_scene().or_else(|| doc.scenes().next()).ok_or_else(|| anyhow!("no scene"))?;
        let mut stack: Vec<(gltf::Node, Mat4)> = scene_root.nodes().map(|n| (n, z_up)).collect();
        while let Some((node, parent)) = stack.pop() {
            let m = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
            if let Some(mesh) = node.mesh() {
                for prim in mesh.primitives() {
                    let reader = prim.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
                    let Some(positions) = reader.read_positions() else { continue };
                    let pos: Vec<Vec3> = positions.map(|p| m.transform_point3(Vec3::from(p))).collect();
                    match reader.read_indices() {
                        Some(idx) => tris.extend(idx.into_u32().filter_map(|i| pos.get(i as usize).copied())),
                        None => tris.extend(pos),
                    }
                }
            }
            stack.extend(node.children().map(|c| (c, m)));
        }
        Ok(tris)
    } else {
        Err(anyhow!("unsupported mesh format"))
    }
}

/// Interleaved (pos 3, normal 3) with flat normals, for meshes placed by a transform.
pub fn interleave_flat(tris: &[Vec3], out: &mut Vec<f32>) {
    for t in tris.chunks_exact(3) {
        let n = (t[1] - t[0]).cross(t[2] - t[0]).normalize_or_zero();
        for p in t {
            out.extend([p.x, p.y, p.z, n.x, n.y, n.z]);
        }
    }
}

/// Interleaved (pos 3, normal 3, albedo 3).
pub fn interleave(mesh: &Mesh, albedo: f32, out: &mut Vec<f32>) {
    for (p, n) in mesh.pos.iter().zip(&mesh.normal) {
        out.extend([p.x, p.y, p.z, n.x, n.y, n.z, albedo, albedo, albedo]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cone_triangles_face_outward() {
        let pos = cone_mesh(24);
        let inside = Vec3::new(0.0, 0.0, 0.7);
        for tri in pos.chunks(3) {
            let n = (tri[1] - tri[0]).cross(tri[2] - tri[0]);
            assert!(n.dot((tri[0] + tri[1] + tri[2]) / 3.0 - inside) > 0.0);
        }
    }

    #[test]
    fn cuboid_normals_point_away_from_center() {
        let m = cuboid(Vec3::splat(-1.0), Vec3::splat(1.0));
        assert_eq!(m.pos.len(), 36);
        for (p, n) in m.pos.iter().zip(&m.normal) {
            assert!(n.dot(*p) > 0.0);
        }
    }
}
