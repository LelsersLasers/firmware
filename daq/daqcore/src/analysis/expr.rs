use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

/// Default maximum gap in milliseconds across which continuous signals may be interpolated
/// or integrated before breaking the series (`f64::NAN` / segment reset).
pub const DEFAULT_MAX_STALENESS_MS: i64 = 250;

/// Default smoothing window in milliseconds for `derivative(sig)` when the second argument is omitted.
pub const DEFAULT_DERIVATIVE_WINDOW_MS: f64 = 20.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnalysisError {
    EmptyExpression,
    Syntax { position: usize, message: String },
    DuplicateChannelId(String),
    EmptyChannelId,
    DependencyCycle(Vec<String>),
    UnknownChannel(String),
    ConfigParse(String),
}

impl fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyExpression => write!(f, "expression must not be empty"),
            Self::Syntax { position, message } => {
                write!(f, "syntax error at position {position}: {message}")
            }
            Self::DuplicateChannelId(id) => {
                write!(f, "duplicate derived channel ID '{id}'")
            }
            Self::EmptyChannelId => write!(f, "derived channel ID must not be empty"),
            Self::DependencyCycle(cycle) => {
                write!(
                    f,
                    "cyclic derived channel dependency: {}",
                    cycle.join(" -> ")
                )
            }
            Self::UnknownChannel(id) => write!(f, "unknown signal or derived channel '{id}'"),
            Self::ConfigParse(msg) => write!(f, "failed to parse derived signals config: {msg}"),
        }
    }
}

impl std::error::Error for AnalysisError {}

/// Canonicalize a signal identifier.
///
/// Canonical CAN signal IDs in Option A v2 use the two-part `msg_name.sig_name` format
/// (e.g., `pack_stats.vbatt_adc`). For backward compatibility with legacy 4-part
/// `BUS.NODE.msg_name.sig_name` identifiers from `plot_factory/`
/// (e.g., `VCAN.A_BOX.pack_stats.vbatt_adc`), 4-segment identifiers are automatically
/// normalized to their trailing `msg_name.sig_name`.
pub fn canonicalize_signal_ref(raw: &str) -> String {
    let trimmed = raw.trim();
    let parts: Vec<&str> = trimmed.split('.').collect();
    if parts.len() == 4 && parts.iter().all(|p| !p.is_empty()) {
        format!("{}.{}", parts[2], parts[3])
    } else {
        trimmed.to_owned()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Pos,
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarFn {
    // 1-argument functions
    Abs,
    Sqrt,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Exp,
    Ln,
    Log10,
    Ceil,
    Floor,
    Round,
    Radians,
    Degrees,
    Sign,
    // 2-argument functions
    Min,
    Max,
    Atan2,
    Pow,
    Hypot,
    // 3-argument functions
    Clamp,
    IfElse,
}

impl ScalarFn {
    fn from_name(name: &str) -> Option<(Self, usize)> {
        match name {
            "abs" => Some((Self::Abs, 1)),
            "sqrt" => Some((Self::Sqrt, 1)),
            "sin" => Some((Self::Sin, 1)),
            "cos" => Some((Self::Cos, 1)),
            "tan" => Some((Self::Tan, 1)),
            "asin" => Some((Self::Asin, 1)),
            "acos" => Some((Self::Acos, 1)),
            "atan" => Some((Self::Atan, 1)),
            "exp" => Some((Self::Exp, 1)),
            "ln" | "log" => Some((Self::Ln, 1)),
            "log10" => Some((Self::Log10, 1)),
            "ceil" => Some((Self::Ceil, 1)),
            "floor" => Some((Self::Floor, 1)),
            "round" => Some((Self::Round, 1)),
            "radians" => Some((Self::Radians, 1)),
            "degrees" => Some((Self::Degrees, 1)),
            "sign" => Some((Self::Sign, 1)),
            "min" => Some((Self::Min, 2)),
            "max" => Some((Self::Max, 2)),
            "atan2" => Some((Self::Atan2, 2)),
            "pow" => Some((Self::Pow, 2)),
            "hypot" => Some((Self::Hypot, 2)),
            "clamp" => Some((Self::Clamp, 3)),
            "ifelse" | "if" => Some((Self::IfElse, 3)),
            _ => None,
        }
    }

    pub fn eval_scalar(self, args: &[f64]) -> f64 {
        let a0 = args.first().copied().unwrap_or(f64::NAN);
        let a1 = args.get(1).copied().unwrap_or(f64::NAN);
        let a2 = args.get(2).copied().unwrap_or(f64::NAN);
        match self {
            Self::Abs => a0.abs(),
            Self::Sqrt => a0.sqrt(),
            Self::Sin => a0.sin(),
            Self::Cos => a0.cos(),
            Self::Tan => a0.tan(),
            Self::Asin => a0.asin(),
            Self::Acos => a0.acos(),
            Self::Atan => a0.atan(),
            Self::Exp => a0.exp(),
            Self::Ln => a0.ln(),
            Self::Log10 => a0.log10(),
            Self::Ceil => a0.ceil(),
            Self::Floor => a0.floor(),
            Self::Round => a0.round(),
            Self::Radians => a0.to_radians(),
            Self::Degrees => a0.to_degrees(),
            Self::Sign => {
                if a0.is_nan() {
                    f64::NAN
                } else if a0 > 0.0 {
                    1.0
                } else if a0 < 0.0 {
                    -1.0
                } else {
                    0.0
                }
            }
            Self::Min => {
                if a0.is_nan() || a1.is_nan() {
                    f64::NAN
                } else {
                    a0.min(a1)
                }
            }
            Self::Max => {
                if a0.is_nan() || a1.is_nan() {
                    f64::NAN
                } else {
                    a0.max(a1)
                }
            }
            Self::Atan2 => a0.atan2(a1),
            Self::Pow => a0.powf(a1),
            Self::Hypot => a0.hypot(a1),
            Self::Clamp => {
                if a0.is_nan() || a1.is_nan() || a2.is_nan() {
                    f64::NAN
                } else {
                    a0.max(a1).min(a2)
                }
            }
            Self::IfElse => {
                if a0.is_nan() {
                    f64::NAN
                } else if a0.abs() > 0.0 {
                    a1
                } else {
                    a2
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SeriesFn {
    /// Smoothed time derivative `d(arg)/dt` in units per second over `window_ms`.
    Derivative { window_ms: f64 },
    /// Trapezoidal time integral `∫ arg dt` in units * seconds, resetting across gaps > `reset_gap_ms`.
    Integral { reset_gap_ms: Option<i64> },
    /// Causal 1st-order RC / Butterworth low-pass filter with cutoff frequency `cutoff_hz`.
    Lowpass { cutoff_hz: f64 },
    /// Causal sliding time-window mean over `[t - window_ms, t]`.
    RollingMean { window_ms: f64 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Literal(f64),
    Signal(String),
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    ScalarCall {
        func: ScalarFn,
        args: Vec<Expr>,
    },
    SeriesCall {
        func: SeriesFn,
        arg: Box<Expr>,
    },
}

impl Expr {
    /// Parse an expression string into an [`Expr`] AST.
    pub fn parse(source: &str) -> Result<Self, AnalysisError> {
        let trimmed = source.trim();
        if trimmed.is_empty() {
            return Err(AnalysisError::EmptyExpression);
        }
        let tokens = tokenize(source)?;
        if tokens.is_empty() {
            return Err(AnalysisError::EmptyExpression);
        }
        let mut parser = Parser {
            tokens: &tokens,
            index: 0,
            depth: 0,
        };
        let expr = parser.parse_or()?;
        if let Some(extra) = parser.peek() {
            return Err(AnalysisError::Syntax {
                position: extra.position,
                message: format!("unexpected token '{}'", extra.text),
            });
        }
        Ok(expr)
    }

    /// Return all unique signal / channel references in order of first appearance.
    pub fn referenced_signals(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        self.collect_refs(&mut out, &mut seen);
        out
    }

    fn collect_refs(&self, out: &mut Vec<String>, seen: &mut BTreeSet<String>) {
        match self {
            Self::Literal(_) => {}
            Self::Signal(name) => {
                if seen.insert(name.clone()) {
                    out.push(name.clone());
                }
            }
            Self::Unary { operand, .. } | Self::SeriesCall { arg: operand, .. } => {
                operand.collect_refs(out, seen);
            }
            Self::Binary { left, right, .. } => {
                left.collect_refs(out, seen);
                right.collect_refs(out, seen);
            }
            Self::ScalarCall { args, .. } => {
                for arg in args {
                    arg.collect_refs(out, seen);
                }
            }
        }
    }

    /// Return `true` if this AST contains any stateful time-series calculus or window function
    /// (`derivative`, `integral`, `lowpass`, `rolling_mean`).
    pub fn contains_series_fn(&self) -> bool {
        match self {
            Self::Literal(_) | Self::Signal(_) => false,
            Self::Unary { operand, .. } => operand.contains_series_fn(),
            Self::Binary { left, right, .. } => {
                left.contains_series_fn() || right.contains_series_fn()
            }
            Self::ScalarCall { args, .. } => args.iter().any(Self::contains_series_fn),
            Self::SeriesCall { .. } => true,
        }
    }

    /// Evaluate the AST over pre-aligned column vectors sharing `timestamps_ms`.
    ///
    /// `columns` must contain a `Vec<f64>` (of length `timestamps_ms.len()`) for every signal
    /// returned by [`Self::referenced_signals`].
    pub fn eval_aligned(
        &self,
        timestamps_ms: &[i64],
        columns: &BTreeMap<String, Vec<f64>>,
        max_staleness_ms: i64,
    ) -> Result<Vec<f64>, AnalysisError> {
        let n = timestamps_ms.len();
        match self {
            Self::Literal(v) => Ok(vec![*v; n]),
            Self::Signal(name) => columns
                .get(name)
                .cloned()
                .ok_or_else(|| AnalysisError::UnknownChannel(name.clone())),
            Self::Unary { op, operand } => {
                let vals = operand.eval_aligned(timestamps_ms, columns, max_staleness_ms)?;
                Ok(vals
                    .into_iter()
                    .map(|x| eval_unary_scalar(*op, x))
                    .collect())
            }
            Self::Binary { op, left, right } => {
                let l_vals = left.eval_aligned(timestamps_ms, columns, max_staleness_ms)?;
                let r_vals = right.eval_aligned(timestamps_ms, columns, max_staleness_ms)?;
                Ok(l_vals
                    .into_iter()
                    .zip(r_vals)
                    .map(|(a, b)| eval_binary_scalar(*op, a, b))
                    .collect())
            }
            Self::ScalarCall { func, args } => {
                let mut eval_args = Vec::with_capacity(args.len());
                for arg in args {
                    eval_args.push(arg.eval_aligned(timestamps_ms, columns, max_staleness_ms)?);
                }
                let mut out = Vec::with_capacity(n);
                let mut scratch = vec![0.0; eval_args.len()];
                for i in 0..n {
                    for (j, col) in eval_args.iter().enumerate() {
                        scratch[j] = col[i];
                    }
                    out.push(func.eval_scalar(&scratch));
                }
                Ok(out)
            }
            Self::SeriesCall { func, arg } => {
                let input = arg.eval_aligned(timestamps_ms, columns, max_staleness_ms)?;
                Ok(eval_series_fn(
                    *func,
                    timestamps_ms,
                    &input,
                    max_staleness_ms,
                ))
            }
        }
    }

    /// Evaluate `(min, max)` envelope bounds over pre-aligned `mean_cols`, `min_cols`, and `max_cols`.
    ///
    /// Pointwise scalar operations propagate interval bounds from child `(min, max)` columns.
    /// Stateful time-series functions (`integral`, `derivative`, `lowpass`, `rolling_mean`)
    /// are evaluated on the `mean` trajectory so per-bucket extremes are never summed cumulatively
    /// into a diverging cone; each bucket's `(min, max)` bounds span the step transition across
    /// that bucket.
    pub fn eval_bounds_aligned(
        &self,
        timestamps_ms: &[i64],
        mean_cols: &BTreeMap<String, Vec<f64>>,
        min_cols: &BTreeMap<String, Vec<f64>>,
        max_cols: &BTreeMap<String, Vec<f64>>,
        max_staleness_ms: i64,
    ) -> Result<(Vec<f64>, Vec<f64>), AnalysisError> {
        let n = timestamps_ms.len();
        match self {
            Self::Literal(v) => Ok((vec![*v; n], vec![*v; n])),
            Self::Signal(name) => {
                let lo = min_cols
                    .get(name)
                    .cloned()
                    .ok_or_else(|| AnalysisError::UnknownChannel(name.clone()))?;
                let hi = max_cols
                    .get(name)
                    .cloned()
                    .ok_or_else(|| AnalysisError::UnknownChannel(name.clone()))?;
                Ok((lo, hi))
            }
            Self::Unary { op, operand } => {
                let (lo, hi) = operand.eval_bounds_aligned(
                    timestamps_ms,
                    mean_cols,
                    min_cols,
                    max_cols,
                    max_staleness_ms,
                )?;
                let mut out_lo = Vec::with_capacity(n);
                let mut out_hi = Vec::with_capacity(n);
                for (a, b) in lo.into_iter().zip(hi) {
                    let va = eval_unary_scalar(*op, a);
                    let vb = eval_unary_scalar(*op, b);
                    out_lo.push(va.min(vb));
                    out_hi.push(va.max(vb));
                }
                Ok((out_lo, out_hi))
            }
            Self::Binary { op, left, right } => {
                let (l_lo, l_hi) = left.eval_bounds_aligned(
                    timestamps_ms,
                    mean_cols,
                    min_cols,
                    max_cols,
                    max_staleness_ms,
                )?;
                let (r_lo, r_hi) = right.eval_bounds_aligned(
                    timestamps_ms,
                    mean_cols,
                    min_cols,
                    max_cols,
                    max_staleness_ms,
                )?;
                let mut out_lo = Vec::with_capacity(n);
                let mut out_hi = Vec::with_capacity(n);
                for i in 0..n {
                    let (lo, hi) =
                        eval_binary_interval_bounds(*op, l_lo[i], l_hi[i], r_lo[i], r_hi[i]);
                    out_lo.push(lo);
                    out_hi.push(hi);
                }
                Ok((out_lo, out_hi))
            }
            Self::ScalarCall { func, args } => {
                let mut arg_los = Vec::with_capacity(args.len());
                let mut arg_his = Vec::with_capacity(args.len());
                for arg in args {
                    let (lo, hi) = arg.eval_bounds_aligned(
                        timestamps_ms,
                        mean_cols,
                        min_cols,
                        max_cols,
                        max_staleness_ms,
                    )?;
                    arg_los.push(lo);
                    arg_his.push(hi);
                }
                let mut out_lo = Vec::with_capacity(n);
                let mut out_hi = Vec::with_capacity(n);
                let mut scratch_lo = vec![0.0; args.len()];
                let mut scratch_hi = vec![0.0; args.len()];
                for i in 0..n {
                    for j in 0..args.len() {
                        scratch_lo[j] = arg_los[j][i];
                        scratch_hi[j] = arg_his[j][i];
                    }
                    let (lo, hi) = eval_scalar_fn_interval_bounds(*func, &scratch_lo, &scratch_hi);
                    out_lo.push(lo);
                    out_hi.push(hi);
                }
                Ok((out_lo, out_hi))
            }
            Self::SeriesCall { func, arg } => {
                let input_mean = arg.eval_aligned(timestamps_ms, mean_cols, max_staleness_ms)?;
                let series_out =
                    eval_series_fn(*func, timestamps_ms, &input_mean, max_staleness_ms);
                let gap_ms = match *func {
                    SeriesFn::Integral { reset_gap_ms } => {
                        reset_gap_ms.unwrap_or(max_staleness_ms).max(1)
                    }
                    _ => max_staleness_ms.max(1),
                };
                Ok(series_step_bounds(timestamps_ms, &series_out, gap_ms))
            }
        }
    }
}

fn eval_unary_scalar(op: UnaryOp, x: f64) -> f64 {
    match op {
        UnaryOp::Pos => x,
        UnaryOp::Neg => -x,
        UnaryOp::Not => {
            if x.is_nan() {
                f64::NAN
            } else if x.abs() > 0.0 {
                0.0
            } else {
                1.0
            }
        }
    }
}

fn eval_binary_interval_bounds(
    op: BinaryOp,
    l_lo: f64,
    l_hi: f64,
    r_lo: f64,
    r_hi: f64,
) -> (f64, f64) {
    if l_lo.is_nan() || l_hi.is_nan() || r_lo.is_nan() || r_hi.is_nan() {
        return (f64::NAN, f64::NAN);
    }
    let (l_min, l_max) = (l_lo.min(l_hi), l_lo.max(l_hi));
    let (r_min, r_max) = (r_lo.min(r_hi), r_lo.max(r_hi));

    match op {
        BinaryOp::Eq => {
            let overlaps = l_min <= r_max && r_min <= l_max;
            if !overlaps {
                return (0.0, 0.0);
            }
            if (l_min - l_max).abs() <= f64::EPSILON && (r_min - r_max).abs() <= f64::EPSILON {
                return (1.0, 1.0);
            }
            return (0.0, 1.0);
        }
        BinaryOp::Ne => {
            let overlaps = l_min <= r_max && r_min <= l_max;
            if !overlaps {
                return (1.0, 1.0);
            }
            if (l_min - l_max).abs() <= f64::EPSILON && (r_min - r_max).abs() <= f64::EPSILON {
                return (0.0, 0.0);
            }
            return (0.0, 1.0);
        }
        _ => {}
    }

    let c1 = eval_binary_scalar(op, l_min, r_min);
    let c2 = eval_binary_scalar(op, l_min, r_max);
    let c3 = eval_binary_scalar(op, l_max, r_min);
    let c4 = eval_binary_scalar(op, l_max, r_max);
    let mut lo = c1.min(c2).min(c3).min(c4);
    let mut hi = c1.max(c2).max(c3).max(c4);

    if matches!(op, BinaryOp::Pow) && l_min <= 0.0 && l_max >= 0.0 && r_max > 0.0 {
        lo = lo.min(0.0);
        hi = hi.max(0.0);
    }

    (lo, hi)
}

fn eval_scalar_fn_interval_bounds(
    func: ScalarFn,
    scratch_lo: &[f64],
    scratch_hi: &[f64],
) -> (f64, f64) {
    if scratch_lo.iter().any(|v| v.is_nan()) || scratch_hi.iter().any(|v| v.is_nan()) {
        return (f64::NAN, f64::NAN);
    }
    match func {
        ScalarFn::Abs if !scratch_lo.is_empty() => {
            let a = scratch_lo[0].min(scratch_hi[0]);
            let b = scratch_lo[0].max(scratch_hi[0]);
            let hi = a.abs().max(b.abs());
            let lo = if a <= 0.0 && b >= 0.0 {
                0.0
            } else {
                a.abs().min(b.abs())
            };
            (lo, hi)
        }
        ScalarFn::Hypot if scratch_lo.len() >= 2 => {
            let (x_min, x_max) = (
                scratch_lo[0].min(scratch_hi[0]),
                scratch_lo[0].max(scratch_hi[0]),
            );
            let (y_min, y_max) = (
                scratch_lo[1].min(scratch_hi[1]),
                scratch_lo[1].max(scratch_hi[1]),
            );
            let x_near = if x_min <= 0.0 && x_max >= 0.0 {
                0.0
            } else {
                x_min.abs().min(x_max.abs())
            };
            let y_near = if y_min <= 0.0 && y_max >= 0.0 {
                0.0
            } else {
                y_min.abs().min(y_max.abs())
            };
            let x_far = x_min.abs().max(x_max.abs());
            let y_far = y_min.abs().max(y_max.abs());
            (x_near.hypot(y_near), x_far.hypot(y_far))
        }
        ScalarFn::Pow if scratch_lo.len() >= 2 => eval_binary_interval_bounds(
            BinaryOp::Pow,
            scratch_lo[0],
            scratch_hi[0],
            scratch_lo[1],
            scratch_hi[1],
        ),
        ScalarFn::IfElse if scratch_lo.len() >= 3 => {
            let c_min = scratch_lo[0].min(scratch_hi[0]);
            let c_max = scratch_lo[0].max(scratch_hi[0]);
            let t_min = scratch_lo[1].min(scratch_hi[1]);
            let t_max = scratch_lo[1].max(scratch_hi[1]);
            let f_min = scratch_lo[2].min(scratch_hi[2]);
            let f_max = scratch_lo[2].max(scratch_hi[2]);
            let can_be_zero = c_min <= 0.0 && c_max >= 0.0;
            let can_be_nonzero = c_min.abs() > 0.0 || c_max.abs() > 0.0;
            match (can_be_nonzero, can_be_zero) {
                (true, false) => (t_min, t_max),
                (false, true) => (f_min, f_max),
                _ => (t_min.min(f_min), t_max.max(f_max)),
            }
        }
        ScalarFn::Sin | ScalarFn::Cos if !scratch_lo.is_empty() => {
            let a = scratch_lo[0].min(scratch_hi[0]);
            let b = scratch_lo[0].max(scratch_hi[0]);
            let va = func.eval_scalar(&[a]);
            let vb = func.eval_scalar(&[b]);
            let mut lo = va.min(vb);
            let mut hi = va.max(vb);
            let shift = if matches!(func, ScalarFn::Sin) {
                std::f64::consts::FRAC_PI_2
            } else {
                0.0
            };
            let two_pi = std::f64::consts::TAU;
            if b - a >= two_pi {
                lo = -1.0;
                hi = 1.0;
            } else {
                let k_max = ((a - shift) / two_pi).ceil();
                if shift + k_max * two_pi <= b {
                    hi = 1.0;
                }
                let k_min = ((a - shift - std::f64::consts::PI) / two_pi).ceil();
                if shift + std::f64::consts::PI + k_min * two_pi <= b {
                    lo = -1.0;
                }
            }
            (lo, hi)
        }
        _ => {
            let v_lo = func.eval_scalar(scratch_lo);
            let v_hi = func.eval_scalar(scratch_hi);
            (v_lo.min(v_hi), v_lo.max(v_hi))
        }
    }
}

fn series_step_bounds(
    timestamps_ms: &[i64],
    values: &[f64],
    max_gap_ms: i64,
) -> (Vec<f64>, Vec<f64>) {
    let n = values.len();
    let mut lo = Vec::with_capacity(n);
    let mut hi = Vec::with_capacity(n);
    for i in 0..n {
        let cur = values[i];
        if i > 0 && cur.is_finite() && values[i - 1].is_finite() {
            let dt = timestamps_ms[i] - timestamps_ms[i - 1];
            if dt > 0 && dt <= max_gap_ms {
                let prev = values[i - 1];
                lo.push(prev.min(cur));
                hi.push(prev.max(cur));
                continue;
            }
        }
        lo.push(cur);
        hi.push(cur);
    }
    (lo, hi)
}

fn eval_binary_scalar(op: BinaryOp, a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    match op {
        BinaryOp::Add => a + b,
        BinaryOp::Sub => a - b,
        BinaryOp::Mul => a * b,
        BinaryOp::Div => {
            if b == 0.0 {
                f64::NAN
            } else {
                a / b
            }
        }
        BinaryOp::Rem => {
            if b == 0.0 {
                f64::NAN
            } else {
                a % b
            }
        }
        BinaryOp::Pow => a.powf(b),
        BinaryOp::Eq => {
            if (a - b).abs() <= f64::EPSILON {
                1.0
            } else {
                0.0
            }
        }
        BinaryOp::Ne => {
            if (a - b).abs() > f64::EPSILON {
                1.0
            } else {
                0.0
            }
        }
        BinaryOp::Lt => {
            if a < b {
                1.0
            } else {
                0.0
            }
        }
        BinaryOp::Le => {
            if a <= b {
                1.0
            } else {
                0.0
            }
        }
        BinaryOp::Gt => {
            if a > b {
                1.0
            } else {
                0.0
            }
        }
        BinaryOp::Ge => {
            if a >= b {
                1.0
            } else {
                0.0
            }
        }
        BinaryOp::And => {
            if a.abs() > 0.0 && b.abs() > 0.0 {
                1.0
            } else {
                0.0
            }
        }
        BinaryOp::Or => {
            if a.abs() > 0.0 || b.abs() > 0.0 {
                1.0
            } else {
                0.0
            }
        }
    }
}

/// Evaluate a time-series calculus or window function over `(timestamps_ms, values)`.
pub fn eval_series_fn(
    func: SeriesFn,
    timestamps_ms: &[i64],
    values: &[f64],
    max_staleness_ms: i64,
) -> Vec<f64> {
    let n = timestamps_ms.len().min(values.len());
    if n == 0 {
        return Vec::new();
    }
    let staleness = max_staleness_ms.max(1);

    match func {
        SeriesFn::Derivative { window_ms } => {
            eval_derivative(timestamps_ms, values, window_ms.max(0.0), staleness)
        }
        SeriesFn::Integral { reset_gap_ms } => {
            let gap_ms = reset_gap_ms.unwrap_or(staleness).max(1);
            eval_integral(timestamps_ms, values, gap_ms)
        }
        SeriesFn::Lowpass { cutoff_hz } => {
            eval_lowpass(timestamps_ms, values, cutoff_hz, staleness)
        }
        SeriesFn::RollingMean { window_ms } => {
            eval_rolling_mean(timestamps_ms, values, window_ms.max(0.0), staleness)
        }
    }
}

/// Find contiguous finite segments `[start..end]` where `values[i].is_finite()` and
/// `timestamps_ms[i] - timestamps_ms[i - 1] <= max_gap_ms`.
fn contiguous_finite_segments(
    timestamps_ms: &[i64],
    values: &[f64],
    max_gap_ms: i64,
) -> Vec<(usize, usize)> {
    let n = timestamps_ms.len().min(values.len());
    let mut segments = Vec::new();
    let mut seg_start: Option<usize> = None;

    for i in 0..n {
        if !values[i].is_finite() {
            if let Some(start) = seg_start.take() {
                segments.push((start, i));
            }
            continue;
        }

        match seg_start {
            Some(start) => {
                let dt = timestamps_ms[i].saturating_sub(timestamps_ms[i - 1]);
                if dt > max_gap_ms || dt < 0 {
                    segments.push((start, i));
                    seg_start = Some(i);
                }
            }
            None => {
                seg_start = Some(i);
            }
        }
    }

    if let Some(start) = seg_start {
        segments.push((start, n));
    }

    segments
}

fn eval_derivative(
    timestamps_ms: &[i64],
    values: &[f64],
    window_ms: f64,
    max_staleness_ms: i64,
) -> Vec<f64> {
    let n = timestamps_ms.len().min(values.len());
    let mut out = vec![f64::NAN; n];
    let half_window_ms = (window_ms * 0.5).round() as i64;

    for (seg_start, seg_end) in contiguous_finite_segments(timestamps_ms, values, max_staleness_ms)
    {
        let seg_len = seg_end - seg_start;
        if seg_len < 2 {
            out[seg_start] = 0.0;
            continue;
        }

        let mut left = seg_start;
        let mut right = seg_start;

        for i in seg_start..seg_end {
            let t_center = timestamps_ms[i];
            while left < i && timestamps_ms[left] < t_center.saturating_sub(half_window_ms) {
                left += 1;
            }
            while right + 1 < seg_end
                && timestamps_ms[right + 1] <= t_center.saturating_add(half_window_ms)
            {
                right += 1;
            }

            // Ensure at least one neighbor on the left or right is included so small
            // windows (or window_ms < sample period) fall back to central/one-sided difference.
            let win_lo = if timestamps_ms[left] == timestamps_ms[right] {
                i.saturating_sub(1).max(seg_start)
            } else {
                left
            };
            let win_hi = if timestamps_ms[win_lo] == timestamps_ms[right] {
                (i + 1).min(seg_end - 1)
            } else {
                right
            };

            let count = (win_hi - win_lo + 1) as f64;
            if count < 2.0 || timestamps_ms[win_hi] == timestamps_ms[win_lo] {
                out[i] = 0.0;
                continue;
            }

            // Ordinary least-squares slope dy/dt (in units per second) over [win_lo..=win_hi].
            let t0 = timestamps_ms[win_lo];
            let mut sum_t = 0.0;
            let mut sum_y = 0.0;
            for j in win_lo..=win_hi {
                sum_t += (timestamps_ms[j] - t0) as f64 / 1000.0;
                sum_y += values[j];
            }
            let mean_t = sum_t / count;
            let mean_y = sum_y / count;

            let mut num = 0.0;
            let mut den = 0.0;
            for j in win_lo..=win_hi {
                let dt = (timestamps_ms[j] - t0) as f64 / 1000.0 - mean_t;
                let dy = values[j] - mean_y;
                num += dt * dy;
                den += dt * dt;
            }

            out[i] = if den > 1e-18 { num / den } else { 0.0 };
        }
    }

    out
}

fn eval_integral(timestamps_ms: &[i64], values: &[f64], reset_gap_ms: i64) -> Vec<f64> {
    let n = timestamps_ms.len().min(values.len());
    let mut out = vec![f64::NAN; n];

    for (seg_start, seg_end) in contiguous_finite_segments(timestamps_ms, values, reset_gap_ms) {
        let mut acc = 0.0;
        out[seg_start] = 0.0;
        for i in (seg_start + 1)..seg_end {
            let dt_s = (timestamps_ms[i] - timestamps_ms[i - 1]).max(0) as f64 / 1000.0;
            acc += 0.5 * (values[i - 1] + values[i]) * dt_s;
            out[i] = acc;
        }
    }

    out
}

fn eval_lowpass(
    timestamps_ms: &[i64],
    values: &[f64],
    cutoff_hz: f64,
    max_staleness_ms: i64,
) -> Vec<f64> {
    let n = timestamps_ms.len().min(values.len());
    let mut out = vec![f64::NAN; n];
    if !cutoff_hz.is_finite() || cutoff_hz <= 0.0 {
        return out;
    }
    let rc = 1.0 / (std::f64::consts::TAU * cutoff_hz);

    for (seg_start, seg_end) in contiguous_finite_segments(timestamps_ms, values, max_staleness_ms)
    {
        let mut state = values[seg_start];
        out[seg_start] = state;
        for i in (seg_start + 1)..seg_end {
            let dt_s = (timestamps_ms[i] - timestamps_ms[i - 1]).max(0) as f64 / 1000.0;
            if dt_s > 0.0 {
                let alpha = dt_s / (rc + dt_s);
                state += alpha * (values[i] - state);
            } else {
                state = values[i];
            }
            out[i] = state;
        }
    }

    out
}

fn eval_rolling_mean(
    timestamps_ms: &[i64],
    values: &[f64],
    window_ms: f64,
    max_staleness_ms: i64,
) -> Vec<f64> {
    let n = timestamps_ms.len().min(values.len());
    let mut out = vec![f64::NAN; n];
    let win_ms = window_ms.round() as i64;

    for (seg_start, seg_end) in contiguous_finite_segments(timestamps_ms, values, max_staleness_ms)
    {
        let mut left = seg_start;
        let mut sum = 0.0;
        for i in seg_start..seg_end {
            sum += values[i];
            let min_t = timestamps_ms[i].saturating_sub(win_ms);
            while left < i && timestamps_ms[left] < min_t {
                sum -= values[left];
                left += 1;
            }
            let count = (i - left + 1) as f64;
            out[i] = sum / count;
        }
    }

    out
}

// ============================================================================
// Derived Signal Definitions & Topological Sort
// ============================================================================

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DerivedSignalDef {
    #[serde(alias = "name")]
    pub id: String,
    #[serde(default)]
    pub label: String,
    pub expression: String,
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub updated_at_ms: i64,
    #[serde(default)]
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DerivedSignalsConfigFile {
    #[serde(alias = "channels", alias = "channel")]
    pub derived_signals: Vec<DerivedSignalDef>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompiledDerivedSignal {
    pub def: DerivedSignalDef,
    pub ast: Expr,
    pub dependencies: Vec<String>,
}

/// Compile and topologically sort a slice of [`DerivedSignalDef`]s so that if channel `B`
/// references channel `A`, `A` appears before `B` regardless of their declaration order.
///
/// Rejects empty IDs, duplicate IDs, syntax errors, and dependency cycles (`A -> B -> A`).
pub fn compile_and_sort_derived_signals(
    defs: &[DerivedSignalDef],
) -> Result<Vec<CompiledDerivedSignal>, AnalysisError> {
    let mut by_id: BTreeMap<String, CompiledDerivedSignal> = BTreeMap::new();

    for def in defs {
        if def.deleted {
            continue;
        }
        let id = def.id.trim().to_owned();
        if id.is_empty() {
            return Err(AnalysisError::EmptyChannelId);
        }
        if by_id.contains_key(&id) {
            return Err(AnalysisError::DuplicateChannelId(id));
        }
        let ast = Expr::parse(&def.expression)?;
        let dependencies = ast.referenced_signals();
        let mut clean_def = def.clone();
        clean_def.id = id.clone();
        if clean_def.label.trim().is_empty() {
            clean_def.label = id.clone();
        }
        by_id.insert(
            id,
            CompiledDerivedSignal {
                def: clean_def,
                ast,
                dependencies,
            },
        );
    }

    // DFS topological sort with cycle path reporting
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum VisitState {
        Visiting,
        Visited,
    }

    let mut state: BTreeMap<String, VisitState> = BTreeMap::new();
    let mut stack: Vec<String> = Vec::new();
    let mut sorted: Vec<CompiledDerivedSignal> = Vec::with_capacity(by_id.len());

    fn visit(
        node_id: &str,
        by_id: &BTreeMap<String, CompiledDerivedSignal>,
        state: &mut BTreeMap<String, VisitState>,
        stack: &mut Vec<String>,
        sorted: &mut Vec<CompiledDerivedSignal>,
    ) -> Result<(), AnalysisError> {
        match state.get(node_id).copied() {
            Some(VisitState::Visited) => return Ok(()),
            Some(VisitState::Visiting) => {
                let start_idx = stack.iter().position(|s| s == node_id).unwrap_or(0);
                let mut cycle: Vec<String> = stack[start_idx..].to_vec();
                cycle.push(node_id.to_owned());
                return Err(AnalysisError::DependencyCycle(cycle));
            }
            None => {}
        }

        state.insert(node_id.to_owned(), VisitState::Visiting);
        stack.push(node_id.to_owned());

        let Some(compiled) = by_id.get(node_id) else {
            stack.pop();
            state.insert(node_id.to_owned(), VisitState::Visited);
            return Ok(());
        };
        for dep in &compiled.dependencies {
            if by_id.contains_key(dep) {
                visit(dep, by_id, state, stack, sorted)?;
            }
        }

        stack.pop();
        state.insert(node_id.to_owned(), VisitState::Visited);
        sorted.push(compiled.clone());
        Ok(())
    }

    // Preserve user declaration order as the primary tie-breaker
    for def in defs {
        if def.deleted {
            continue;
        }
        let id = def.id.trim();
        visit(id, &by_id, &mut state, &mut stack, &mut sorted)?;
    }

    Ok(sorted)
}

// ============================================================================
// Tokenizer & Recursive-Descent Parser
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
enum TokenKind {
    Number(f64),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
    StarStar,
    Bang,
    EqEq,
    BangEq,
    Lt,
    Le,
    Gt,
    Ge,
    AndAnd,
    OrOr,
    LParen,
    RParen,
    Comma,
}

#[derive(Debug, Clone)]
struct Token {
    kind: TokenKind,
    text: String,
    position: usize,
}

fn is_ident_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

fn is_ident_continue(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn tokenize(source: &str) -> Result<Vec<Token>, AnalysisError> {
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut pos = 0;
    let mut tokens = Vec::new();

    while pos < len {
        let ch = bytes[pos] as char;
        if ch.is_ascii_whitespace() {
            pos += 1;
            continue;
        }

        let start = pos;

        // Number literal: 123, 123.45, .5, 1.5e-3
        if ch.is_ascii_digit()
            || (ch == '.' && pos + 1 < len && (bytes[pos + 1] as char).is_ascii_digit())
        {
            while pos < len && (bytes[pos] as char).is_ascii_digit() {
                pos += 1;
            }
            if pos < len && bytes[pos] == b'.' {
                pos += 1;
                while pos < len && (bytes[pos] as char).is_ascii_digit() {
                    pos += 1;
                }
            }
            if pos < len && (bytes[pos] == b'e' || bytes[pos] == b'E') {
                pos += 1;
                if pos < len && (bytes[pos] == b'+' || bytes[pos] == b'-') {
                    pos += 1;
                }
                let exp_start = pos;
                while pos < len && (bytes[pos] as char).is_ascii_digit() {
                    pos += 1;
                }
                if pos == exp_start {
                    return Err(AnalysisError::Syntax {
                        position: start,
                        message: format!(
                            "invalid scientific notation in '{}'",
                            &source[start..pos]
                        ),
                    });
                }
            }
            let text = &source[start..pos];
            let val = text.parse::<f64>().map_err(|_| AnalysisError::Syntax {
                position: start,
                message: format!("invalid number literal '{text}'"),
            })?;
            tokens.push(Token {
                kind: TokenKind::Number(val),
                text: text.to_owned(),
                position: start,
            });
            continue;
        }

        // Identifier (with optional dot-separated segments, e.g. `pack_stats.vbatt_adc`)
        if is_ident_start(ch) {
            pos += 1;
            while pos < len && is_ident_continue(bytes[pos] as char) {
                pos += 1;
            }
            while pos < len && bytes[pos] == b'.' {
                pos += 1;
                let seg_start = pos;
                while pos < len && is_ident_continue(bytes[pos] as char) {
                    pos += 1;
                }
                if pos == seg_start {
                    return Err(AnalysisError::Syntax {
                        position: start,
                        message: format!("trailing '.' in identifier '{}'", &source[start..pos]),
                    });
                }
            }
            let text = &source[start..pos];
            tokens.push(Token {
                kind: TokenKind::Ident(text.to_owned()),
                text: text.to_owned(),
                position: start,
            });
            continue;
        }

        // Multi-character operators
        if pos + 1 < len {
            let two = &source[pos..pos + 2];
            let kind = match two {
                "**" => Some(TokenKind::StarStar),
                "==" => Some(TokenKind::EqEq),
                "!=" => Some(TokenKind::BangEq),
                "<=" => Some(TokenKind::Le),
                ">=" => Some(TokenKind::Ge),
                "&&" => Some(TokenKind::AndAnd),
                "||" => Some(TokenKind::OrOr),
                _ => None,
            };
            if let Some(kind) = kind {
                tokens.push(Token {
                    kind,
                    text: two.to_owned(),
                    position: start,
                });
                pos += 2;
                continue;
            }
        }

        // Single-character operators and punctuation
        let kind = match ch {
            '+' => Some(TokenKind::Plus),
            '-' => Some(TokenKind::Minus),
            '*' => Some(TokenKind::Star),
            '/' => Some(TokenKind::Slash),
            '%' => Some(TokenKind::Percent),
            '^' => Some(TokenKind::Caret),
            '!' => Some(TokenKind::Bang),
            '<' => Some(TokenKind::Lt),
            '>' => Some(TokenKind::Gt),
            '(' => Some(TokenKind::LParen),
            ')' => Some(TokenKind::RParen),
            ',' => Some(TokenKind::Comma),
            _ => None,
        };

        if let Some(kind) = kind {
            tokens.push(Token {
                kind,
                text: ch.to_string(),
                position: start,
            });
            pos += 1;
        } else {
            return Err(AnalysisError::Syntax {
                position: start,
                message: format!("unexpected character '{ch}'"),
            });
        }
    }

    Ok(tokens)
}

const MAX_PARSE_DEPTH: usize = 128;

struct Parser<'a> {
    tokens: &'a [Token],
    index: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn enter_recursion(&mut self, position: usize) -> Result<(), AnalysisError> {
        if self.depth >= MAX_PARSE_DEPTH {
            return Err(AnalysisError::Syntax {
                position,
                message: format!("expression nesting exceeds maximum depth ({MAX_PARSE_DEPTH})"),
            });
        }
        self.depth += 1;
        Ok(())
    }

    fn exit_recursion(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.index)
    }

    fn advance(&mut self) -> Option<&'a Token> {
        let tok = self.tokens.get(self.index)?;
        self.index += 1;
        Some(tok)
    }

    fn end_pos(&self) -> usize {
        self.tokens.last().map_or(0, |t| t.position + t.text.len())
    }

    fn parse_or(&mut self) -> Result<Expr, AnalysisError> {
        let mut node = self.parse_and()?;
        while let Some(tok) = self.peek() {
            if tok.kind == TokenKind::OrOr {
                self.advance();
                let right = self.parse_and()?;
                node = Expr::Binary {
                    op: BinaryOp::Or,
                    left: Box::new(node),
                    right: Box::new(right),
                };
            } else {
                break;
            }
        }
        Ok(node)
    }

    fn parse_and(&mut self) -> Result<Expr, AnalysisError> {
        let mut node = self.parse_comparison()?;
        while let Some(tok) = self.peek() {
            if tok.kind == TokenKind::AndAnd {
                self.advance();
                let right = self.parse_comparison()?;
                node = Expr::Binary {
                    op: BinaryOp::And,
                    left: Box::new(node),
                    right: Box::new(right),
                };
            } else {
                break;
            }
        }
        Ok(node)
    }

    fn parse_comparison(&mut self) -> Result<Expr, AnalysisError> {
        let mut node = self.parse_additive()?;
        while let Some(tok) = self.peek() {
            let op = match tok.kind {
                TokenKind::EqEq => Some(BinaryOp::Eq),
                TokenKind::BangEq => Some(BinaryOp::Ne),
                TokenKind::Lt => Some(BinaryOp::Lt),
                TokenKind::Le => Some(BinaryOp::Le),
                TokenKind::Gt => Some(BinaryOp::Gt),
                TokenKind::Ge => Some(BinaryOp::Ge),
                _ => None,
            };
            if let Some(op) = op {
                self.advance();
                let right = self.parse_additive()?;
                node = Expr::Binary {
                    op,
                    left: Box::new(node),
                    right: Box::new(right),
                };
            } else {
                break;
            }
        }
        Ok(node)
    }

    fn parse_additive(&mut self) -> Result<Expr, AnalysisError> {
        let mut node = self.parse_multiplicative()?;
        while let Some(tok) = self.peek() {
            let op = match tok.kind {
                TokenKind::Plus => Some(BinaryOp::Add),
                TokenKind::Minus => Some(BinaryOp::Sub),
                _ => None,
            };
            if let Some(op) = op {
                self.advance();
                let right = self.parse_multiplicative()?;
                node = Expr::Binary {
                    op,
                    left: Box::new(node),
                    right: Box::new(right),
                };
            } else {
                break;
            }
        }
        Ok(node)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, AnalysisError> {
        let mut node = self.parse_unary()?;
        while let Some(tok) = self.peek() {
            let op = match tok.kind {
                TokenKind::Star => Some(BinaryOp::Mul),
                TokenKind::Slash => Some(BinaryOp::Div),
                TokenKind::Percent => Some(BinaryOp::Rem),
                _ => None,
            };
            if let Some(op) = op {
                self.advance();
                let right = self.parse_unary()?;
                node = Expr::Binary {
                    op,
                    left: Box::new(node),
                    right: Box::new(right),
                };
            } else {
                break;
            }
        }
        Ok(node)
    }

    fn parse_unary(&mut self) -> Result<Expr, AnalysisError> {
        if let Some(tok) = self.peek() {
            let op = match tok.kind {
                TokenKind::Plus => Some(UnaryOp::Pos),
                TokenKind::Minus => Some(UnaryOp::Neg),
                TokenKind::Bang => Some(UnaryOp::Not),
                _ => None,
            };
            if let Some(op) = op {
                let pos = tok.position;
                self.advance();
                self.enter_recursion(pos)?;
                let operand = self.parse_unary();
                self.exit_recursion();
                return Ok(Expr::Unary {
                    op,
                    operand: Box::new(operand?),
                });
            }
        }
        self.parse_power()
    }

    fn parse_power(&mut self) -> Result<Expr, AnalysisError> {
        let node = self.parse_primary()?;
        if let Some(tok) = self.peek()
            && matches!(tok.kind, TokenKind::Caret | TokenKind::StarStar)
        {
            let pos = tok.position;
            self.advance();
            // Right-associative exponentiation: `2 ^ 3 ^ 2 == 2 ^ (3 ^ 2)` and `-2 ^ 2 == -(2 ^ 2)`
            self.enter_recursion(pos)?;
            let right = self.parse_unary();
            self.exit_recursion();
            return Ok(Expr::Binary {
                op: BinaryOp::Pow,
                left: Box::new(node),
                right: Box::new(right?),
            });
        }
        Ok(node)
    }

    fn parse_primary(&mut self) -> Result<Expr, AnalysisError> {
        let Some(tok) = self.advance() else {
            return Err(AnalysisError::Syntax {
                position: self.end_pos(),
                message: "unexpected end of expression".to_owned(),
            });
        };

        match &tok.kind {
            TokenKind::Number(val) => Ok(Expr::Literal(*val)),
            TokenKind::LParen => {
                self.enter_recursion(tok.position)?;
                let inner = self.parse_or();
                self.exit_recursion();
                let inner = inner?;
                match self.advance() {
                    Some(close) if close.kind == TokenKind::RParen => Ok(inner),
                    Some(other) => Err(AnalysisError::Syntax {
                        position: other.position,
                        message: format!("expected ')', found '{}'", other.text),
                    }),
                    None => Err(AnalysisError::Syntax {
                        position: self.end_pos(),
                        message: "expected ')' before end of expression".to_owned(),
                    }),
                }
            }
            TokenKind::Ident(ident) => {
                if self
                    .peek()
                    .is_some_and(|next| next.kind == TokenKind::LParen)
                {
                    self.enter_recursion(tok.position)?;
                    let res = self.parse_function_call(ident, tok.position);
                    self.exit_recursion();
                    res
                } else {
                    Ok(Expr::Signal(canonicalize_signal_ref(ident)))
                }
            }
            _ => Err(AnalysisError::Syntax {
                position: tok.position,
                message: format!("expected expression, found '{}'", tok.text),
            }),
        }
    }

    fn parse_function_call(&mut self, name: &str, call_pos: usize) -> Result<Expr, AnalysisError> {
        // Consume '('
        self.advance();

        if self.peek().is_some_and(|t| t.kind == TokenKind::RParen) {
            return Err(AnalysisError::Syntax {
                position: call_pos,
                message: format!("function '{name}()' requires at least 1 argument"),
            });
        }

        let mut args = vec![self.parse_or()?];
        while self.peek().is_some_and(|t| t.kind == TokenKind::Comma) {
            self.advance();
            args.push(self.parse_or()?);
        }

        match self.advance() {
            Some(close) if close.kind == TokenKind::RParen => {}
            Some(other) => {
                return Err(AnalysisError::Syntax {
                    position: other.position,
                    message: format!(
                        "expected ',' or ')' in call to '{name}()', found '{}'",
                        other.text
                    ),
                });
            }
            None => {
                return Err(AnalysisError::Syntax {
                    position: self.end_pos(),
                    message: format!("unclosed call to '{name}('"),
                });
            }
        }

        // Check time-series window/calculus functions first
        match name {
            "derivative" => {
                let window_ms = match args.len() {
                    1 => DEFAULT_DERIVATIVE_WINDOW_MS,
                    2 => extract_const_number(&args[1], name, "window_ms", call_pos)?,
                    n => {
                        return Err(AnalysisError::Syntax {
                            position: call_pos,
                            message: format!(
                                "derivative() expects 1 or 2 arguments (signal[, window_ms]), got {n}"
                            ),
                        });
                    }
                };
                if window_ms < 0.0 {
                    return Err(AnalysisError::Syntax {
                        position: call_pos,
                        message: "derivative() window_ms must be >= 0".to_owned(),
                    });
                }
                let first_arg = args.into_iter().next().ok_or_else(|| AnalysisError::Syntax {
                    position: call_pos,
                    message: "derivative() missing signal argument".to_owned(),
                })?;
                return Ok(Expr::SeriesCall {
                    func: SeriesFn::Derivative { window_ms },
                    arg: Box::new(first_arg),
                });
            }
            "integral" => {
                let reset_gap_ms = match args.len() {
                    1 => None,
                    2 => {
                        let gap = extract_const_number(&args[1], name, "reset_gap_ms", call_pos)?;
                        if gap <= 0.0 {
                            return Err(AnalysisError::Syntax {
                                position: call_pos,
                                message: "integral() reset_gap_ms must be > 0".to_owned(),
                            });
                        }
                        Some(gap.round() as i64)
                    }
                    n => {
                        return Err(AnalysisError::Syntax {
                            position: call_pos,
                            message: format!(
                                "integral() expects 1 or 2 arguments (signal[, reset_gap_ms]), got {n}"
                            ),
                        });
                    }
                };
                let first_arg = args.into_iter().next().ok_or_else(|| AnalysisError::Syntax {
                    position: call_pos,
                    message: "integral() missing signal argument".to_owned(),
                })?;
                return Ok(Expr::SeriesCall {
                    func: SeriesFn::Integral { reset_gap_ms },
                    arg: Box::new(first_arg),
                });
            }
            "lowpass" => {
                if args.len() != 2 {
                    return Err(AnalysisError::Syntax {
                        position: call_pos,
                        message: format!(
                            "lowpass() expects 2 arguments (signal, cutoff_hz), got {}",
                            args.len()
                        ),
                    });
                }
                let cutoff_hz = extract_const_number(&args[1], name, "cutoff_hz", call_pos)?;
                if cutoff_hz <= 0.0 {
                    return Err(AnalysisError::Syntax {
                        position: call_pos,
                        message: "lowpass() cutoff_hz must be > 0".to_owned(),
                    });
                }
                let first_arg = args.into_iter().next().ok_or_else(|| AnalysisError::Syntax {
                    position: call_pos,
                    message: "lowpass() missing signal argument".to_owned(),
                })?;
                return Ok(Expr::SeriesCall {
                    func: SeriesFn::Lowpass { cutoff_hz },
                    arg: Box::new(first_arg),
                });
            }
            "rolling_mean" => {
                if args.len() != 2 {
                    return Err(AnalysisError::Syntax {
                        position: call_pos,
                        message: format!(
                            "rolling_mean() expects 2 arguments (signal, window_ms), got {}",
                            args.len()
                        ),
                    });
                }
                let window_ms = extract_const_number(&args[1], name, "window_ms", call_pos)?;
                if window_ms < 0.0 {
                    return Err(AnalysisError::Syntax {
                        position: call_pos,
                        message: "rolling_mean() window_ms must be >= 0".to_owned(),
                    });
                }
                let first_arg = args.into_iter().next().ok_or_else(|| AnalysisError::Syntax {
                    position: call_pos,
                    message: "rolling_mean() missing signal argument".to_owned(),
                })?;
                return Ok(Expr::SeriesCall {
                    func: SeriesFn::RollingMean { window_ms },
                    arg: Box::new(first_arg),
                });
            }
            _ => {}
        }

        // Check scalar functions
        let Some((func, expected_arity)) = ScalarFn::from_name(name) else {
            return Err(AnalysisError::Syntax {
                position: call_pos,
                message: format!("unknown function '{name}()'"),
            });
        };
        if args.len() != expected_arity {
            return Err(AnalysisError::Syntax {
                position: call_pos,
                message: format!(
                    "{name}() expects {expected_arity} argument(s), got {}",
                    args.len()
                ),
            });
        }

        Ok(Expr::ScalarCall { func, args })
    }
}

fn extract_const_number(
    expr: &Expr,
    fn_name: &str,
    param_name: &str,
    position: usize,
) -> Result<f64, AnalysisError> {
    match expr {
        Expr::Literal(v) if v.is_finite() => Ok(*v),
        Expr::Unary {
            op: UnaryOp::Pos,
            operand,
        } => extract_const_number(operand, fn_name, param_name, position),
        Expr::Unary {
            op: UnaryOp::Neg,
            operand,
        } => Ok(-extract_const_number(
            operand, fn_name, param_name, position,
        )?),
        _ => Err(AnalysisError::Syntax {
            position,
            message: format!("{fn_name}() parameter '{param_name}' must be a numeric literal"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_arithmetic_precedence_and_canonicalizes_legacy_refs() {
        let expr =
            Expr::parse("VCAN.A_BOX.pack_stats.vbatt_adc * 0.14659808276 + -2 ^ 3 * (4 - 1)")
                .expect("parse valid expression");

        assert_eq!(
            expr.referenced_signals(),
            vec!["pack_stats.vbatt_adc".to_owned()]
        );

        let mut cols = BTreeMap::new();
        cols.insert("pack_stats.vbatt_adc".to_owned(), vec![1000.0, 2000.0]);
        let ts = vec![1000, 1010];
        let out = expr
            .eval_aligned(&ts, &cols, 250)
            .expect("eval valid expression");

        // -2^3 * 3 = -8 * 3 = -24
        assert!((out[0] - (146.59808276 - 24.0)).abs() < 1e-6);
        assert!((out[1] - (293.19616552 - 24.0)).abs() < 1e-6);
    }

    #[test]
    fn evaluates_scalar_and_comparison_functions() {
        let expr = Expr::parse("ifelse(speed_mps > 2.0, clamp(angle_deg / 4.5, -10.0, 10.0), 0.0)")
            .expect("parse scalar call");

        let ts = vec![0, 100, 200];
        let mut cols = BTreeMap::new();
        cols.insert("speed_mps".to_owned(), vec![1.0, 10.0, 15.0]);
        cols.insert("angle_deg".to_owned(), vec![45.0, 22.5, -90.0]);

        let out = expr
            .eval_aligned(&ts, &cols, 250)
            .expect("eval scalar call");
        assert_eq!(out, vec![0.0, 5.0, -10.0]);
    }

    #[test]
    fn evaluates_derivative_integral_lowpass_and_rolling_mean_with_gaps() {
        // Two contiguous segments separated by a 5,000 ms gap:
        // Segment 1: t = 0, 100, 200, 300 ms -> position = 0, 10, 20, 30 mm (constant velocity = 100 mm/s)
        // Segment 2: t = 5300, 5400, 5500 ms -> position = 100, 80, 60 mm (constant velocity = -200 mm/s)
        let ts = vec![0, 100, 200, 300, 5300, 5400, 5500];
        let pos = vec![0.0, 10.0, 20.0, 30.0, 100.0, 80.0, 60.0];
        let mut cols = BTreeMap::new();
        cols.insert("shock.fl_mm".to_owned(), pos);

        let deriv_expr = Expr::parse("derivative(shock.fl_mm, 200)").expect("parse derivative");
        let deriv = deriv_expr
            .eval_aligned(&ts, &cols, 250)
            .expect("eval derivative");
        for &v in &deriv[0..4] {
            assert!((v - 100.0).abs() < 1e-6, "expected +100 mm/s, got {v}");
        }
        // Notice no spike across the 5s jump from 30 mm to 100 mm!
        for &v in &deriv[4..7] {
            assert!((v - (-200.0)).abs() < 1e-6, "expected -200 mm/s, got {v}");
        }

        // Integral of constant 10.0 over 0..300ms (0.3s) -> 0, 1, 2, 3, then resets after the 5s gap!
        cols.insert(
            "power_w".to_owned(),
            vec![10.0, 10.0, 10.0, 10.0, 20.0, 20.0, 20.0],
        );
        let int_expr = Expr::parse("integral(power_w)").expect("parse integral");
        let integrated = int_expr
            .eval_aligned(&ts, &cols, 250)
            .expect("eval integral");
        assert!((integrated[0] - 0.0).abs() < 1e-9);
        assert!((integrated[1] - 1.0).abs() < 1e-9);
        assert!((integrated[2] - 2.0).abs() < 1e-9);
        assert!((integrated[3] - 3.0).abs() < 1e-9);
        // Reset at t = 5300 ms
        assert!((integrated[4] - 0.0).abs() < 1e-9);
        assert!((integrated[5] - 2.0).abs() < 1e-9);
        assert!((integrated[6] - 4.0).abs() < 1e-9);

        // Rolling mean over 150ms window
        let rm_expr = Expr::parse("rolling_mean(shock.fl_mm, 150)").expect("parse rolling_mean");
        let rm = rm_expr
            .eval_aligned(&ts, &cols, 250)
            .expect("eval rolling_mean");
        assert!((rm[0] - 0.0).abs() < 1e-9);
        assert!((rm[1] - 5.0).abs() < 1e-9);
        assert!((rm[2] - 15.0).abs() < 1e-9);
        assert!((rm[3] - 25.0).abs() < 1e-9);
        assert!((rm[4] - 100.0).abs() < 1e-9);
    }

    #[test]
    fn topological_sort_orders_out_of_order_definitions_and_detects_cycles() {
        let defs = vec![
            DerivedSignalDef {
                id: "battery.power_kw".to_owned(),
                label: "Battery Power (kW)".to_owned(),
                expression: "battery.power_w / 1000.0".to_owned(),
                unit: "kW".to_owned(),
                ..Default::default()
            },
            DerivedSignalDef {
                id: "deleted.channel".to_owned(),
                label: "Deleted Channel".to_owned(),
                expression: "nonexistent.sig * 2.0".to_owned(),
                unit: "V".to_owned(),
                deleted: true,
                ..Default::default()
            },
            DerivedSignalDef {
                id: "battery.power_w".to_owned(),
                label: "Battery Power (W)".to_owned(),
                expression: "battery.voltage_v * pack_stats.pack_current".to_owned(),
                unit: "W".to_owned(),
                ..Default::default()
            },
            DerivedSignalDef {
                id: "battery.voltage_v".to_owned(),
                label: "Battery Voltage (V)".to_owned(),
                expression: "pack_stats.vbatt_adc * 0.14659808276".to_owned(),
                unit: "V".to_owned(),
                ..Default::default()
            },
        ];

        let sorted = compile_and_sort_derived_signals(&defs).expect("sort DAG");
        let ids: Vec<&str> = sorted.iter().map(|c| c.def.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["battery.voltage_v", "battery.power_w", "battery.power_kw"]
        );

        let cyclic = vec![
            DerivedSignalDef {
                id: "a".to_owned(),
                label: String::new(),
                expression: "b + 1".to_owned(),
                unit: String::new(),
                ..Default::default()
            },
            DerivedSignalDef {
                id: "b".to_owned(),
                label: String::new(),
                expression: "a + 1".to_owned(),
                unit: String::new(),
                ..Default::default()
            },
        ];
        let err = compile_and_sort_derived_signals(&cyclic).expect_err("cycle should fail");
        assert!(matches!(err, AnalysisError::DependencyCycle(_)));
    }

    #[test]
    fn interval_bounds_capture_zero_crossings_for_hypot_and_pow() {
        let hypot_expr = Expr::parse("hypot(ax, ay)").expect("parse hypot");
        let pow_expr = Expr::parse("ax ^ 2").expect("parse pow");

        let ts = vec![1000];
        let mut mean_cols = BTreeMap::new();
        let mut min_cols = BTreeMap::new();
        let mut max_cols = BTreeMap::new();
        mean_cols.insert("ax".to_owned(), vec![0.0]);
        mean_cols.insert("ay".to_owned(), vec![0.0]);
        min_cols.insert("ax".to_owned(), vec![-2.0]);
        max_cols.insert("ax".to_owned(), vec![1.0]);
        min_cols.insert("ay".to_owned(), vec![-1.0]);
        max_cols.insert("ay".to_owned(), vec![2.0]);

        let (h_lo, h_hi) = hypot_expr
            .eval_bounds_aligned(&ts, &mean_cols, &min_cols, &max_cols, 250)
            .expect("eval hypot bounds");
        assert!((h_lo[0] - 0.0).abs() < 1e-9);
        assert!((h_hi[0] - 8.0_f64.sqrt()).abs() < 1e-9);

        let (p_lo, p_hi) = pow_expr
            .eval_bounds_aligned(&ts, &mean_cols, &min_cols, &max_cols, 250)
            .expect("eval pow bounds");
        assert!((p_lo[0] - 0.0).abs() < 1e-9);
        assert!((p_hi[0] - 4.0).abs() < 1e-9);
    }
}
