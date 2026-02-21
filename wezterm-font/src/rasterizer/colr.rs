// Cairo was stripped from Terminaler (Windows-only build).
// This file retains the data type definitions used by hbwrap.rs and ftwrap.rs.
// The actual cairo rendering functions are removed; callers bail at runtime
// if color-font rendering is attempted.
use wezterm_color_types::SrgbaPixel;

#[derive(Clone, Debug)]
pub struct ColorStop {
    pub offset: f64,
    pub color: SrgbaPixel,
}

/// Stub for cairo::Extend.
#[derive(Clone, Debug, PartialEq)]
pub enum Extend {
    Pad,
    Repeat,
    Reflect,
    None,
}

/// Stub for cairo::Operator (composite mode).
#[derive(Clone, Debug)]
pub enum Operator {
    Clear,
    Source,
    Over,
    In,
    Out,
    Atop,
    Dest,
    DestOver,
    DestIn,
    DestOut,
    DestAtop,
    Xor,
    Add,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Multiply,
    HslHue,
    HslSaturation,
    HslColor,
    HslLuminosity,
}

/// Stub for cairo::Matrix (affine transform).
#[derive(Clone, Debug)]
pub struct Matrix {
    pub xx: f64,
    pub yx: f64,
    pub xy: f64,
    pub yy: f64,
    pub x0: f64,
    pub y0: f64,
}

impl Matrix {
    pub fn identity() -> Self {
        Self {
            xx: 1.0,
            yx: 0.0,
            xy: 0.0,
            yy: 1.0,
            x0: 0.0,
            y0: 0.0,
        }
    }

    pub fn new(xx: f64, yx: f64, xy: f64, yy: f64, x0: f64, y0: f64) -> Self {
        Self {
            xx,
            yx,
            xy,
            yy,
            x0,
            y0,
        }
    }

    pub fn translate(&mut self, tx: f64, ty: f64) {
        self.x0 += tx;
        self.y0 += ty;
    }

    pub fn scale(&mut self, sx: f64, sy: f64) {
        self.xx *= sx;
        self.xy *= sx;
        self.x0 *= sx;
        self.yx *= sy;
        self.yy *= sy;
        self.y0 *= sy;
    }

    pub fn rotate(&mut self, angle: f64) {
        let cos_a = angle.cos();
        let sin_a = angle.sin();
        let xx = self.xx * cos_a - self.yx * sin_a;
        let yx = self.xx * sin_a + self.yx * cos_a;
        let xy = self.xy * cos_a - self.yy * sin_a;
        let yy = self.xy * sin_a + self.yy * cos_a;
        self.xx = xx;
        self.yx = yx;
        self.xy = xy;
        self.yy = yy;
    }
}

#[derive(Clone, Debug)]
pub struct ColorLine {
    pub color_stops: Vec<ColorStop>,
    pub extend: Extend,
}

#[derive(Debug, Clone)]
pub enum PaintOp {
    PushTransform(Matrix),
    PopTransform,
    PushClip(Vec<DrawOp>),
    PopClip,
    PaintSolid(SrgbaPixel),
    PaintLinearGradient {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        color_line: ColorLine,
    },
    PaintRadialGradient {
        x0: f32,
        y0: f32,
        r0: f32,
        x1: f32,
        y1: f32,
        r1: f32,
        color_line: ColorLine,
    },
    PaintSweepGradient {
        x0: f32,
        y0: f32,
        start_angle: f32,
        end_angle: f32,
        color_line: ColorLine,
    },
    PushGroup,
    PopGroup(Operator),
}

#[derive(Debug, Clone)]
pub enum DrawOp {
    MoveTo {
        to_x: f32,
        to_y: f32,
    },
    LineTo {
        to_x: f32,
        to_y: f32,
    },
    QuadTo {
        control_x: f32,
        control_y: f32,
        to_x: f32,
        to_y: f32,
    },
    CubicTo {
        control1_x: f32,
        control1_y: f32,
        control2_x: f32,
        control2_y: f32,
        to_x: f32,
        to_y: f32,
    },
    ClosePath,
}
