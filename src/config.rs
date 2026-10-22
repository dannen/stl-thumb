use image::ImageFormat;
use std::f32;
use std::path::Path;

// Largest thumbnail side, in pixels. Common GPUs limit textures to this.
pub const MAX_SIZE: u32 = 16384;

#[derive(Clone)]
pub struct Material {
    pub ambient: [f32; 3],
    pub diffuse: [f32; 3],
    pub specular: [f32; 3],
}

#[derive(Clone)]
pub enum AAMethod {
    None,
    FXAA,
}

#[derive(Clone)]
pub struct Config {
    pub model_filename: String,
    pub img_filename: String,
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
    pub visible: bool,
    pub verbosity: usize,
    pub material: Material,
    pub background: (f32, f32, f32, f32),
    pub aamethod: AAMethod,
    pub recalc_normals: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            model_filename: "".to_string(),
            img_filename: "".to_string(),
            format: ImageFormat::Png,
            width: 1024,
            height: 768,
            visible: false,
            verbosity: 0,
            material: Material {
                ambient: [0.00, 0.13, 0.26],
                diffuse: [0.38, 0.63, 1.00],
                specular: [1.00, 1.00, 1.00],
            },
            background: (0.0, 0.0, 0.0, 0.0),
            aamethod: AAMethod::FXAA,
            recalc_normals: false,
        }
    }
}

impl Config {
    pub fn new() -> Config {
        // Define command line arguments
        let mut matches = clap::Command::new(env!("CARGO_PKG_NAME"))
            .version(env!("CARGO_PKG_VERSION"))
            .author(env!("CARGO_PKG_AUTHORS"))
            .arg(
                clap::Arg::new("MODEL_FILE")
                    .help("3D model file (STL, OBJ or 3MF). Use - to read from stdin instead of a file.")
                    .required(true)
                    .index(1),
            )
            .arg(
                clap::Arg::new("IMG_FILE")
                    .help("Thumbnail image file. Use - to write to stdout instead of a file.")
                    .required(true)
                    .index(2),
            )
            .arg(
                clap::Arg::new("format")
                    .help("The format of the image file. If not specified it will be determined from the file extension, or default to PNG if there is no extension. Supported formats: PNG, JPEG, GIF, ICO, BMP")
                    .short('f')
                    .long("format")
                    .action(clap::ArgAction::Set)
            )
            .arg(
                clap::Arg::new("size")
                    .help("Size of thumbnail (square)")
                    .short('s')
                    .long("size")
                    .action(clap::ArgAction::Set)
                    .value_parser(clap::value_parser!(u32).range(1..=MAX_SIZE as i64))
                    .required(false)
            )
            .arg(
                clap::Arg::new("visible")
                    .help("Display the thumbnail in a window instead of saving a file")
                    .short('x')
                    .action(clap::ArgAction::SetTrue)
            )
            .arg(
                clap::Arg::new("verbosity")
                    .short('v')
                    .action(clap::ArgAction::Count)
                    .help("Increase message verbosity")
            )
            .arg(
                clap::Arg::new("material")
                    .help("Colors for rendering the mesh using the Phong reflection model. Requires 3 colors as rgb hex values: ambient, diffuse, and specular. Defaults to blue.")
                    .short('m')
                    .long("material")
                    .value_names(["ambient","diffuse","specular"])
                    .value_parser(html_to_rgb)
            )
            .arg(
                clap::Arg::new("background")
                    .help("The background color with transparency (rgba hex, or rgb hex for opaque). Default is 00000000 (transparent). JPEG images, which have no transparency, show transparent parts as white.")
                    .short('b')
                    .long("background")
                    .action(clap::ArgAction::Set)
                    .value_parser(html_to_rgba)
                    .required(false)
            )
            .arg(
                clap::Arg::new("aamethod")
                    .help("Anti-aliasing method. Default is FXAA, which is fast but may introduce artifacts.")
                    .short('a')
                    .long("antialiasing")
                    .value_parser(["none", "fxaa"]),
            )
            .arg(
                clap::Arg::new("recalc_normals")
                    .help("Force recalculation of face normals. Use when dealing with malformed STL files.")
                    .long("recalc-normals")
                    .action(clap::ArgAction::SetTrue)
            )
            .get_matches();

        let mut c = Config {
            ..Default::default()
        };

        c.model_filename = matches
            .remove_one::<String>("MODEL_FILE")
            .expect("MODEL_FILE not provided");
        c.img_filename = matches
            .remove_one::<String>("IMG_FILE")
            .expect("IMG_FILE not provided");
        match matches.get_one::<String>("format") {
            Some(x) => c.format = match_format(x),
            None => {
                if let Some(ext) = Path::new(&c.img_filename)
                    .extension()
                    .and_then(|e| e.to_str())
                {
                    c.format = match_format(ext);
                }
            }
        };

        if let Some(&x) = matches.get_one::<u32>("size") {
            c.width = x;
            c.height = x;
        }

        c.visible = matches.get_flag("visible");
        c.verbosity = matches.get_count("verbosity") as usize;
        if let Some(materials) = matches.get_many::<[f32; 3]>("material") {
            let mut iter = materials.copied();
            c.material = Material {
                ambient: iter.next().unwrap_or([0.0, 0.0, 0.0]),
                diffuse: iter.next().unwrap_or([0.0, 0.0, 0.0]),
                specular: iter.next().unwrap_or([0.0, 0.0, 0.0]),
            };
        }
        if let Some(&x) = matches.get_one::<(f32, f32, f32, f32)>("background") {
            c.background = x;
        }
        if let Some(x) = matches.get_one::<String>("aamethod") {
            match x.as_str() {
                "none" => c.aamethod = AAMethod::None,
                "fxaa" => c.aamethod = AAMethod::FXAA,
                _ => unreachable!(),
            }
        }
        c.recalc_normals = matches.get_flag("recalc_normals");

        c
    }
}

fn match_format(ext: &str) -> ImageFormat {
    match ext.to_lowercase().as_str() {
        "png" => ImageFormat::Png,
        "jpeg" | "jpg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "ico" => ImageFormat::Ico,
        "bmp" => ImageFormat::Bmp,
        _ => {
            warn!("Unsupported image format. Using PNG instead.");
            ImageFormat::Png
        }
    }
}

// Parse "rrggbb" or "rrggbbaa" (optionally starting with #) into 0.0-1.0 channels
fn parse_hex_color(color: &str) -> Result<Vec<f32>, String> {
    let hex = color.strip_prefix('#').unwrap_or(color);
    if !(hex.len() == 6 || hex.len() == 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "invalid color {:?}: expected hex digits like ff8800 or ff880080",
            color
        ));
    }
    Ok((0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap() as f32 / 255.0)
        .collect())
}

fn html_to_rgb(color: &str) -> Result<[f32; 3], String> {
    match parse_hex_color(color)?[..] {
        [r, g, b] => Ok([r, g, b]),
        _ => Err(format!(
            "invalid color {:?}: expected rgb hex like ff8800",
            color
        )),
    }
}

fn html_to_rgba(color: &str) -> Result<(f32, f32, f32, f32), String> {
    match parse_hex_color(color)?[..] {
        [r, g, b] => Ok((r, g, b, 1.0)),
        [r, g, b, a] => Ok((r, g, b, a)),
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_colors() {
        assert_eq!(html_to_rgb("ff0000").unwrap(), [1.0, 0.0, 0.0]);
        assert_eq!(html_to_rgb("#00ff00").unwrap(), [0.0, 1.0, 0.0]);
        assert_eq!(html_to_rgba("0000ff").unwrap(), (0.0, 0.0, 1.0, 1.0));
        assert_eq!(html_to_rgba("ffffff00").unwrap(), (1.0, 1.0, 1.0, 0.0));
        assert!(html_to_rgb("fff").is_err());
        assert!(html_to_rgb("ff0000ff").is_err());
        assert!(html_to_rgba("zzzzzz").is_err());
        assert!(html_to_rgba("éééé").is_err());
    }
}
