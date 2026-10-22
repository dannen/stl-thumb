extern crate ahash;
extern crate cgmath;
extern crate stl_io;
extern crate tobj;

use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::io::{Cursor, Read, Seek};
use std::path::Path;
use std::{fmt, io};

use self::cgmath::{InnerSpace, SquareMatrix, Transform};
use self::stl_io::{Triangle, Vector};

use self::ahash::AHashMap;
use self::tobj::LoadOptions;

// The parsers need Seek, so stdin is read into memory first. This caps how much.
const MAX_STDIN_BYTES: u64 = 1 << 30;
// 3MF components can reuse objects, so a small file can expand into a huge mesh
const MAX_3MF_TRIANGLES: usize = 20_000_000;
const MAX_3MF_NESTING: usize = 16;

#[derive(Copy, Clone)]
pub struct Vertex {
    position: [f32; 3],
    //texcoords: [f32; 2],
}

implement_vertex!(Vertex, position);
//implement_vertex!(Vertex, position, texcoords);

#[derive(Debug, Copy, Clone)]
pub struct Normal {
    normal: [f32; 3],
}

implement_vertex!(Normal, normal);

#[derive(Clone)]
pub struct BoundingBox {
    pub min: cgmath::Point3<f32>,
    pub max: cgmath::Point3<f32>,
}

impl BoundingBox {
    // Contains nothing until the first vertex is added
    fn empty() -> BoundingBox {
        BoundingBox {
            min: cgmath::Point3 {
                x: f32::INFINITY,
                y: f32::INFINITY,
                z: f32::INFINITY,
            },
            max: cgmath::Point3 {
                x: f32::NEG_INFINITY,
                y: f32::NEG_INFINITY,
                z: f32::NEG_INFINITY,
            },
        }
    }
    fn expand(&mut self, vert: &stl_io::Vertex) {
        self.min.x = self.min.x.min(vert[0]);
        self.max.x = self.max.x.max(vert[0]);
        self.min.y = self.min.y.min(vert[1]);
        self.max.y = self.max.y.max(vert[1]);
        self.min.z = self.min.z.min(vert[2]);
        self.max.z = self.max.z.max(vert[2]);
    }
    pub fn center(&self) -> cgmath::Point3<f32> {
        cgmath::Point3 {
            x: (self.min.x + self.max.x) / 2.0,
            y: (self.min.y + self.max.y) / 2.0,
            z: (self.min.z + self.max.z) / 2.0,
        }
    }
    fn length(&self) -> f32 {
        self.max.x - self.min.x
    }
    fn width(&self) -> f32 {
        self.max.y - self.min.y
    }
    fn height(&self) -> f32 {
        self.max.z - self.min.z
    }
}

impl fmt::Display for BoundingBox {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "X: {}, {}", self.min.x, self.max.x)?;
        writeln!(f, "Y: {}, {}", self.min.y, self.max.y)?;
        writeln!(f, "Z: {}, {}", self.min.z, self.max.z)?;
        Ok(())
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
enum Format {
    Stl,
    Obj,
    ThreeMf,
}

impl Format {
    fn from_extension(path: &Path) -> Option<Format> {
        match path
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("")
            .to_lowercase()
            .as_str()
        {
            "obj" => Some(Format::Obj),
            "stl" => Some(Format::Stl),
            "3mf" => Some(Format::ThreeMf),
            _ => None,
        }
    }

    // Guess the format from the start of the file and its total length
    fn detect(head: &[u8], len: u64) -> Result<Format, Box<dyn Error>> {
        // 3MF files are ZIP archives
        if head.starts_with(b"PK\x03\x04") {
            return Ok(Format::ThreeMf);
        }
        // Binary STL: 80 byte header, triangle count, 50 bytes per triangle
        if head.len() >= 84 {
            let count = u32::from_le_bytes([head[80], head[81], head[82], head[83]]) as u64;
            if len == 84 + 50 * count {
                return Ok(Format::Stl);
            }
        }
        let text = String::from_utf8_lossy(head);
        if text.trim_start().to_lowercase().starts_with("solid") {
            return Ok(Format::Stl);
        }
        if text
            .lines()
            .any(|l| l.starts_with("v ") || l.starts_with("f "))
        {
            return Ok(Format::Obj);
        }
        Err("Unrecognized model format. Supported formats: STL, OBJ, 3MF".into())
    }
}

#[derive(Clone)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub normals: Vec<Normal>,
    pub indices: Vec<usize>,
    pub bounds: BoundingBox,
    model_had_normals: bool,
}

impl Mesh {
    fn empty(model_had_normals: bool) -> Mesh {
        Mesh {
            vertices: Vec::new(),
            normals: Vec::new(),
            indices: Vec::new(),
            bounds: BoundingBox::empty(),
            model_had_normals,
        }
    }

    // Load mesh data from file (if provided) or stdin
    pub fn load(model_filename: &str, recalc_normals: bool) -> Result<Mesh, Box<dyn Error>> {
        // TODO: Add support for URIs instead of plain file names
        // https://developer.gnome.org/integration-guide/stable/thumbnailer.html.en
        match model_filename {
            "-" => {
                let mut input_buffer = Vec::new();
                io::stdin()
                    .take(MAX_STDIN_BYTES + 1)
                    .read_to_end(&mut input_buffer)?;
                if input_buffer.len() as u64 > MAX_STDIN_BYTES {
                    return Err(
                        format!("Input on stdin is larger than {} bytes", MAX_STDIN_BYTES).into(),
                    );
                }
                let format = Format::detect(&input_buffer, input_buffer.len() as u64)?;
                Mesh::from_format(format, Cursor::new(input_buffer), recalc_normals)
            }
            _ => {
                let model_filename = Path::new(model_filename);
                let model_file = File::open(model_filename)?;
                let len = model_file.metadata()?.len();
                let mut model_file = BufReader::new(model_file);
                let format = match Format::from_extension(model_filename) {
                    Some(format) => format,
                    None => Format::detect(model_file.fill_buf()?, len)?,
                };
                Mesh::from_format(format, model_file, recalc_normals)
            }
        }
    }

    fn from_format<R>(
        format: Format,
        mut model: R,
        recalc_normals: bool,
    ) -> Result<Mesh, Box<dyn Error>>
    where
        R: BufRead + Seek,
    {
        if model.fill_buf()?.is_empty() {
            return Err("Model file is empty".into());
        }
        let mesh = match format {
            Format::Obj => Mesh::from_obj(model, recalc_normals)?,
            Format::Stl => Mesh::from_stl(model, recalc_normals)?,
            Format::ThreeMf => Mesh::from_3mf(model, recalc_normals)?,
        };
        if mesh.vertices.is_empty() {
            return Err("Model has no triangles".into());
        }
        Ok(mesh)
    }

    pub fn from_3mf<R>(model_file: R, _recalc_normals: bool) -> Result<Mesh, Box<dyn Error>>
    where
        R: Read + Seek,
    {
        let models = threemf::read(model_file)?;

        let mut mesh = Mesh::empty(false);

        // Combine all the models into a single mesh.
        for model in &models {
            if model.build.item.is_empty() {
                // Part files that another model refers to have no build items: draw their meshes as they are
                for object in &model.resources.object {
                    if let Some(object_mesh) = &object.mesh {
                        mesh.add_3mf_mesh(object_mesh, &cgmath::Matrix4::identity())?;
                    }
                }
            } else {
                let objects: AHashMap<usize, &threemf::model::Object> = model
                    .resources
                    .object
                    .iter()
                    .map(|object| (object.id, object))
                    .collect();
                for item in &model.build.item {
                    mesh.add_3mf_object(
                        &objects,
                        item.objectid,
                        &transform_3mf(&item.transform),
                        0,
                    )?;
                }
            }
        }

        Ok(mesh)
    }

    // Add an object and, through its components, the objects it is made of
    fn add_3mf_object(
        &mut self,
        objects: &AHashMap<usize, &threemf::model::Object>,
        id: usize,
        transform: &cgmath::Matrix4<f64>,
        depth: usize,
    ) -> Result<(), Box<dyn Error>> {
        if depth > MAX_3MF_NESTING {
            return Err("3MF components are nested too deeply".into());
        }
        let object = match objects.get(&id) {
            Some(object) => object,
            None => {
                // Objects in other model files (production extension) are drawn with those files
                warn!("3MF build refers to object {} outside this model", id);
                return Ok(());
            }
        };
        if let Some(object_mesh) = &object.mesh {
            self.add_3mf_mesh(object_mesh, transform)?;
        }
        if let Some(components) = &object.components {
            for component in &components.component {
                let transform = transform * transform_3mf(&component.transform);
                self.add_3mf_object(objects, component.objectid, &transform, depth + 1)?;
            }
        }
        Ok(())
    }

    fn add_3mf_mesh(
        &mut self,
        mesh: &threemf::model::Mesh,
        transform: &cgmath::Matrix4<f64>,
    ) -> Result<(), Box<dyn Error>> {
        if self.vertices.len() / 3 + mesh.triangles.triangle.len() > MAX_3MF_TRIANGLES {
            return Err(format!("3MF model has more than {} triangles", MAX_3MF_TRIANGLES).into());
        }
        // A mirroring transform turns the triangles inside out
        let mirrored = transform.determinant() < 0.0;
        let vertex = |index: usize| -> Result<stl_io::Vertex, Box<dyn Error>> {
            let v = mesh
                .vertices
                .vertex
                .get(index)
                .ok_or("3MF triangle refers to a missing vertex")?;
            let p = transform.transform_point(cgmath::Point3::new(v.x, v.y, v.z));
            Ok(stl_io::Vertex::new([p.x as f32, p.y as f32, p.z as f32]))
        };
        for triangle in &mesh.triangles.triangle {
            let (v2, v3) = if mirrored {
                (triangle.v3, triangle.v2)
            } else {
                (triangle.v2, triangle.v3)
            };
            // Re-use `Mesh::process_tri`, which creates new vertices for every
            // triangle.
            // Possible optimization: re-use triangles instead.
            let triangle = stl_io::Triangle {
                normal: stl_io::Normal::new([0f32, 0f32, 0f32]),
                vertices: [vertex(triangle.v1)?, vertex(v2)?, vertex(v3)?],
            };
            self.process_tri(&triangle, true);
        }
        Ok(())
    }

    pub fn from_stl<R>(mut model_file: R, recalc_normals: bool) -> Result<Mesh, Box<dyn Error>>
    where
        R: Read + Seek,
    {
        //let model = stl_io::read_stl(&mut model_file)?;
        //debug!("{:?}", model);
        let stl_iter = stl_io::create_stl_reader(&mut model_file)?;

        let mut mesh = Mesh::empty(true);

        let mut face_count = 0;
        for triangle in stl_iter {
            mesh.process_tri(&triangle?, recalc_normals);
            face_count += 1;
            //debug!("{:?}",triangle);
        }

        if !mesh.model_had_normals {
            warn!("STL file missing surface normals");
        }
        info!("Bounds:");
        info!("{}", mesh.bounds);
        info!("Center:\t{:?}", mesh.bounds.center());
        info!("Triangles processed:\t{}\n", face_count);

        Ok(mesh)
    }

    pub fn from_obj<R>(mut obj_file: R, recalc_normals: bool) -> Result<Mesh, Box<dyn Error>>
    where
        R: BufRead,
    {
        let (models, _) = tobj::load_obj_buf(
            &mut obj_file,
            &LoadOptions {
                single_index: true,
                triangulate: true,
                ..LoadOptions::default()
            },
            |_| Ok((Vec::new(), AHashMap::new())),
        )?;
        let mut mesh = Mesh::empty(true);
        for model in &models {
            let tri_idx = &model.mesh.indices;
            let p = &model.mesh.positions;
            let n = &model.mesh.normals;
            let use_file_normals = !recalc_normals && !n.is_empty() && n.len() == p.len();
            for tri in tri_idx.chunks_exact(3) {
                let index = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
                if index.iter().any(|&i| i * 3 + 2 >= p.len()) {
                    return Err("OBJ face refers to a missing vertex".into());
                }

                let vertices = index.map(|i| Vector::new([p[i * 3], p[i * 3 + 1], p[i * 3 + 2]]));
                for v in vertices.iter() {
                    mesh.bounds.expand(v);
                    mesh.vertices.push(Vertex {
                        position: (*v).into(),
                    });
                    //debug!("{:?}", v);
                }

                let normals = if use_file_normals {
                    index.map(|i| Normal {
                        normal: [n[i * 3], n[i * 3 + 1], n[i * 3 + 2]],
                    })
                } else {
                    let n = normal(&Triangle {
                        vertices,
                        normal: Vector::new([0.0, 0.0, 0.0]),
                    });
                    [n, n, n]
                };
                mesh.normals.extend_from_slice(&normals);
            }
        }
        Ok(mesh)
    }

    fn process_tri(&mut self, tri: &stl_io::Triangle, recalc_normals: bool) {
        for v in tri.vertices {
            self.bounds.expand(&v);
            self.vertices.push(Vertex { position: v.into() });
            //debug!("{:?}", v);
        }
        // Use normal from STL file if it is provided, otherwise calculate it ourselves
        let n: Normal;
        if recalc_normals || (tri.normal == stl_io::Vector::new([0.0, 0.0, 0.0])) {
            self.model_had_normals = false;
            n = normal(tri);
        } else {
            n = Normal {
                normal: tri.normal.into(),
            };
        }
        //debug!("{:?}",tri.normal);
        // TODO: Figure out how to get away with 1 normal instead of 3
        for _ in 0..3 {
            self.normals.push(n);
        }
    }

    // Move the mesh to be centered at the origin
    // and scaled to fit a 2 x 2 x 2 box. This means that
    // all coordinates will be between -1.0 and 1.0
    pub fn scale_and_center(&self) -> cgmath::Matrix4<f32> {
        // Move center to origin
        let center = self.bounds.center();
        let translation_vector = cgmath::Vector3::new(-center.x, -center.y, -center.z);
        let translation_matrix = cgmath::Matrix4::from_translation(translation_vector);
        // Scale
        let longest = self
            .bounds
            .length()
            .max(self.bounds.width())
            .max(self.bounds.height());
        // A model that is a single point has no size to scale by
        let scale = if longest > 0.0 && longest.is_finite() {
            2.0 / longest
        } else {
            1.0
        };
        info!("Scale:\t{}", scale);
        let scale_matrix = cgmath::Matrix4::from_scale(scale);
        scale_matrix * translation_matrix
    }
}

impl fmt::Display for Mesh {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "Verts: {}", self.vertices.len())?;
        writeln!(f, "Norms: {}", self.normals.len())?;
        //writeln!(f, "Tex Coords: {:?}", geometry.tex_coords)?;
        writeln!(f, "Indices: {:?}", self.indices.len())?;
        writeln!(f)?;
        Ok(())
    }
}

// 3MF transforms are 3x4 matrices in row-vector order: "m00 m01 m02 m10 ... m32".
// Read as columns, that is the usual column-vector matrix.
fn transform_3mf(transform: &Option<[f64; 12]>) -> cgmath::Matrix4<f64> {
    match transform {
        Some(t) => cgmath::Matrix4::new(
            t[0], t[1], t[2], 0.0, t[3], t[4], t[5], 0.0, t[6], t[7], t[8], 0.0, t[9], t[10],
            t[11], 1.0,
        ),
        None => cgmath::Matrix4::identity(),
    }
}

// Calculate surface normal of triangle using cross product
// TODO: The GPU can probably do this a lot faster than we can.
// See if there is an option for offloading this.
// Probably need to use a geometry shader (not supported in Opengl ES).
fn normal(tri: &stl_io::Triangle) -> Normal {
    let p1 = cgmath::Vector3::new(tri.vertices[0][0], tri.vertices[0][1], tri.vertices[0][2]);
    let p2 = cgmath::Vector3::new(tri.vertices[1][0], tri.vertices[1][1], tri.vertices[1][2]);
    let p3 = cgmath::Vector3::new(tri.vertices[2][0], tri.vertices[2][1], tri.vertices[2][2]);
    let v = p2 - p1;
    let w = p3 - p1;
    let n = v.cross(w);
    let mag = n.magnitude();
    // Triangles with no area have no direction (and draw no pixels)
    if !(mag > 0.0 && mag.is_finite()) {
        return Normal {
            normal: [0.0, 0.0, 0.0],
        };
    }
    Normal {
        normal: [n.x / mag, n.y / mag, n.z / mag],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_formats() {
        let mut binary = vec![0u8; 84 + 50];
        binary[80] = 1;
        assert_eq!(
            Format::detect(&binary, binary.len() as u64).unwrap(),
            Format::Stl
        );
        let ascii = b"solid cube\nfacet normal 0 0 1\n";
        assert_eq!(
            Format::detect(ascii, ascii.len() as u64).unwrap(),
            Format::Stl
        );
        let obj = b"# cube\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n";
        assert_eq!(Format::detect(obj, obj.len() as u64).unwrap(), Format::Obj);
        let zip = b"PK\x03\x04rest";
        assert_eq!(
            Format::detect(zip, zip.len() as u64).unwrap(),
            Format::ThreeMf
        );
        assert!(Format::detect(b"nonsense", 8).is_err());
    }

    #[test]
    fn stl_without_triangles_is_an_error() {
        let stl = b"solid x\nendsolid x\n".to_vec();
        assert!(Mesh::from_format(Format::Stl, Cursor::new(stl), false).is_err());
    }

    #[test]
    fn obj_with_bad_index_is_an_error() {
        let obj = b"v 0 0 0\nv 1 0 0\nf 1 2 9\n".to_vec();
        assert!(Mesh::from_format(Format::Obj, Cursor::new(obj), false).is_err());
    }

    #[test]
    fn single_point_scales_safely() {
        let stl = b"solid x\nfacet normal 0 0 0\nouter loop\nvertex 1 1 1\nvertex 1 1 1\nvertex 1 1 1\nendloop\nendfacet\nendsolid x\n".to_vec();
        let mesh = Mesh::from_format(Format::Stl, Cursor::new(stl), false).unwrap();
        let m: [[f32; 4]; 4] = mesh.scale_and_center().into();
        assert!(m.iter().flatten().all(|x| x.is_finite()));
        assert_eq!(mesh.normals[0].normal, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn transform_3mf_translates() {
        let t = transform_3mf(&Some([
            1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 5.0, 6.0, 7.0,
        ]));
        let p = t.transform_point(cgmath::Point3::new(1.0, 2.0, 3.0));
        assert_eq!((p.x, p.y, p.z), (6.0, 8.0, 10.0));
    }
}
