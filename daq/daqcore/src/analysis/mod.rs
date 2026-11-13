pub mod align;
pub mod distance;
pub mod expr;

pub use align::{
    AlignedXY, AlignedXYZ, TargetGrid, align_series_map, align_xy_pairs, align_xyz_triples,
    resample_continuous_linear, resample_discrete_hold, series_to_timestamp_values,
};
pub use distance::{
    DEFAULT_SPEED_INTEGRATION_GAP_MS, DistanceSeries, compute_distance_axis,
    compute_distance_axis_with_max_gap, integrate_velocity_samples, map_series_to_distance,
};
pub use expr::{
    AnalysisError, BinaryOp, CompiledDerivedSignal, DEFAULT_DERIVATIVE_WINDOW_MS,
    DEFAULT_MAX_STALENESS_MS, DerivedSignalDef, DerivedSignalsConfigFile, Expr, ScalarFn, SeriesFn,
    UnaryOp, canonicalize_signal_ref, compile_and_sort_derived_signals, eval_series_fn,
};
